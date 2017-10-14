// Copyright 2016 The RLS Project Developers.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(type_ascription)]

#[macro_use]
extern crate derive_new;
#[macro_use]
extern crate log;
extern crate rls_data as data;
extern crate rls_span as span;
extern crate rustc_serialize;

mod analysis;
mod raw;
mod loader;
mod lowering;
mod listings;
mod util;
#[cfg(test)]
mod test;

pub use analysis::Def;
use analysis::Analysis;
pub use raw::{name_space_for_def_kind, read_analysis_incremental, DefKind, Target};
pub use loader::{AnalysisLoader, CargoAnalysisLoader};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};
use std::u64;

#[derive(Debug)]
pub struct AnalysisHost<L: AnalysisLoader = CargoAnalysisLoader> {
    analysis: Mutex<Option<Analysis>>,
    master_crate_map: Mutex<HashMap<String, u32>>,
    loader: L,
}

pub type AResult<T> = Result<T, AError>;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AError {
    MutexPoison,
    Unclassified,
}

#[derive(Debug, Clone)]
pub struct SymbolResult {
    pub id: Id,
    pub name: String,
    pub kind: raw::DefKind,
    pub span: Span,
}

impl SymbolResult {
    fn new(id: Id, def: &Def) -> SymbolResult {
        SymbolResult {
            id: id,
            name: def.name.clone(),
            span: def.span.clone(),
            kind: def.kind,
        }
    }
}

pub type Span = span::Span<span::ZeroIndexed>;

#[derive(Clone, Eq, PartialEq, Debug, Hash, new)]
pub struct CrateId {
    pub name: String,
    pub path: PathBuf
}

#[derive(Copy, Clone, Eq, PartialEq, Debug, Hash, new)]
pub struct Id(u64);

// Used to indicate a missing index in the Id.
pub const NULL: Id = Id(u64::MAX);

type Blacklist<'a> = &'a [&'static str];

macro_rules! clone_field {
    ($field: ident) => { |x| x.$field.clone() }
}

macro_rules! def_span {
    ($analysis: expr, $id: expr) => {
        $analysis.with_defs_and_then($id, |def| Some(def.span.clone()))
    }
}

impl AnalysisHost<CargoAnalysisLoader> {
    pub fn new(target: Target) -> AnalysisHost {
        AnalysisHost {
            analysis: Mutex::new(None),
            master_crate_map: Mutex::new(HashMap::new()),
            loader: CargoAnalysisLoader {
                path_prefix: Mutex::new(None),
                target: target,
            },
        }
    }
}

impl<L: AnalysisLoader> AnalysisHost<L> {
    pub fn new_with_loader(l: L) -> AnalysisHost<L> {
        AnalysisHost {
            analysis: Mutex::new(None),
            master_crate_map: Mutex::new(HashMap::new()),
            loader: l,
        }
    }

    /// Reloads given data passed in `analysis`. This will first check and read
    /// on-disk data (just like `reload`). It then imports the data we're
    /// passing in directly.
    pub fn reload_from_analysis(
        &self,
        analysis: data::Analysis,
        path_prefix: &Path,
        base_dir: &Path,
        blacklist: Blacklist,
    ) -> AResult<()> {
        self.reload_with_blacklist(path_prefix, base_dir, blacklist)?;

        let shiit = analysis.prelude.as_ref().map(|x| x.crate_root.clone()).unwrap();
        warn!("CRATE_ROOT: {} (from direct analysis)", shiit);
        // FIXME

        lowering::lower(
            vec![raw::Crate::new(analysis, SystemTime::now(), PathBuf::from("GOWNO"))],
            base_dir,
            self,
            |host, per_crate, id| {
                let mut a = host.analysis.lock()?;
                a.as_mut().unwrap().update(id, per_crate);
                Ok(())
            },
        )
    }

    pub fn reload(&self, path_prefix: &Path, base_dir: &Path) -> AResult<()> {
        self.reload_with_blacklist(path_prefix, base_dir, &[])
    }

    pub fn reload_with_blacklist(
        &self,
        path_prefix: &Path,
        base_dir: &Path,
        blacklist: Blacklist,
    ) -> AResult<()> {
        trace!(
            "reload_with_blacklist {:?} {:?} {:?}",
            path_prefix,
            base_dir,
            blacklist
        );
        let empty = {
            let a = self.analysis.lock()?;
            a.is_none()
        };
        if empty || self.loader.needs_hard_reload(path_prefix) {
            return self.hard_reload_with_blacklist(path_prefix, base_dir, blacklist);
        }

        let timestamps = {
            let a = self.analysis.lock()?;
            a.as_ref().unwrap().timestamps()
        };

        let raw_analysis = read_analysis_incremental(&self.loader, timestamps, blacklist);

        let result = lowering::lower(raw_analysis, base_dir, self, |host, per_crate, id| {
            let mut a = host.analysis.lock()?;
            a.as_mut().unwrap().update(id, per_crate);
            Ok(())
        });
        result
    }

    // Reloads the entire project's analysis data.
    pub fn hard_reload(&self, path_prefix: &Path, base_dir: &Path) -> AResult<()> {
        self.hard_reload_with_blacklist(path_prefix, base_dir, &[])
    }

    pub fn hard_reload_with_blacklist(
        &self,
        path_prefix: &Path,
        base_dir: &Path,
        blacklist: Blacklist,
    ) -> AResult<()> {
        trace!("hard_reload {:?} {:?}", path_prefix, base_dir);
        self.loader.set_path_prefix(path_prefix);
        let raw_analysis = read_analysis_incremental(&self.loader, HashMap::new(), blacklist);

        // We're going to create a dummy AnalysisHost that we will fill with data,
        // then once we're done, we'll swap its data into self.
        let mut fresh_host = self.loader.fresh_host();
        fresh_host.analysis = Mutex::new(Some(Analysis::new()));
        let lowering_result = lowering::lower(
            raw_analysis,
            base_dir,
            &fresh_host,
            |host, per_crate, crate_id| {
                host.analysis
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .per_crate
                    .insert(crate_id, per_crate);
                Ok(())
            },
        );

        if let Err(s) = lowering_result {
            let mut a = self.analysis.lock()?;
            *a = None;
            return Err(s);
        }

        {
            let mut mcm = self.master_crate_map.lock()?;
            *mcm = fresh_host.master_crate_map.into_inner().unwrap();
        }

        let mut a = self.analysis.lock()?;
        *a = Some(fresh_host.analysis.into_inner().unwrap().unwrap());
        Ok(())
    }

    /// Note that self.has_def == true =/> self.goto_def.is_some(), since if the
    /// def is in an api crate, there is no reasonable span to jump to.
    pub fn has_def(&self, id: Id) -> bool {
        match self.analysis.lock() {
            Ok(a) => a.as_ref().unwrap().has_def(id),
            _ => false,
        }
    }

    pub fn get_def(&self, id: Id) -> AResult<Def> {
        self.with_analysis(|a| a.with_defs(id, |def| def.clone()))
    }

    pub fn goto_def(&self, span: &Span) -> AResult<Span> {
        self.with_analysis(|a| a.def_id_for_span(span).and_then(|id| def_span!(a, id)))
    }

    pub fn for_each_child_def<F, T>(&self, id: Id, f: F) -> AResult<Vec<T>>
    where
        F: FnMut(Id, &Def) -> T,
    {
        self.with_analysis(|a| a.for_each_child(id, f))
    }

    pub fn def_parents(&self, id: Id) -> AResult<Vec<(Id, String)>> {
        self.with_analysis(|a| {
            let mut result = vec![];
            let mut next = id;
            loop {
                match a.with_defs_and_then(next, |def| {
                    def.parent
                        .and_then(|p| a.with_defs(p, |def| (p, def.name.clone())))
                }) {
                    Some((id, name)) => {
                        result.insert(0, (id, name));
                        next = id;
                    }
                    None => {
                        return Some(result);
                    }
                }
            }
        })
    }

    /// Returns the name of each crate in the program and the id of the root
    /// module of that crate.
    pub fn def_roots(&self) -> AResult<Vec<(Id, String)>> {
        self.with_analysis(|a| {
            Some(
                a.for_all_crates(|c| c.root_id.map(|id| vec![(id, c.name.clone())])),
            )
        })
    }

    pub fn id(&self, span: &Span) -> AResult<Id> {
        self.with_analysis(|a| a.def_id_for_span(span))
    }

    /// Like id, but will only return a value if it is in the same crate as span.
    pub fn crate_local_id(&self, span: &Span) -> AResult<Id> {
        self.with_analysis(|a| {
            a.for_each_crate(|c| {
                c.def_id_for_span
                    .get(span)
                    .and_then(|id| if c.defs.contains_key(id) {
                        Some(id)
                    } else {
                        None
                    })
                    .cloned()
            })
        })
    }

    pub fn find_all_refs(&self, span: &Span, include_decl: bool) -> AResult<Vec<Span>> {
        let t_start = Instant::now();
        let result = if include_decl {
            self.with_analysis(|a| {
                a.def_id_for_span(span).and_then(|id| {
                    a.with_ref_spans(id, |refs| {
                        def_span!(a, id)
                            .into_iter()
                            .chain(refs.iter().cloned())
                            .collect::<Vec<_>>()
                    }).or_else(|| def_span!(a, id).map(|s| vec![s]))
                })
            })
        } else {
            self.with_analysis(|a| {
                a.def_id_for_span(span).map(|id| {
                    a.with_ref_spans(id, |refs| refs.clone())
                        .unwrap_or_else(Vec::new)
                })
            })
        };

        let time = t_start.elapsed();
        info!(
            "find_all_refs: {}s",
            time.as_secs() as f64 + time.subsec_nanos() as f64 / 1_000_000_000.0
        );
        result
    }

    pub fn show_type(&self, span: &Span) -> AResult<String> {
        self.with_analysis(|a| {
            a.def_id_for_span(span)
                .and_then(|id| a.with_defs(id, clone_field!(value)))
                .or_else(|| a.with_globs(span, clone_field!(value)))
        })
    }

    pub fn docs(&self, span: &Span) -> AResult<String> {
        self.with_analysis(|a| {
            a.def_id_for_span(span)
                .and_then(|id| a.with_defs(id, clone_field!(docs)))
        })
    }

    pub fn name_defs(&self, name: &str) -> AResult<Vec<Def>> {
        let t_start = Instant::now();
        let result = self.with_analysis(|a| {
            let defs = a.defs_for_name(name);
            info!("defs_for_name {:?}", &defs);
            Some(defs)
        });

        let time = t_start.elapsed();
        info!(
            "name_defs: {}",
            time.as_secs() as f64 + time.subsec_nanos() as f64 / 1_000_000_000.0
        );

        result
    }

    /// Search for a symbol name, returns a list of spans matching defs and refs
    /// for that name.
    pub fn search(&self, name: &str) -> AResult<Vec<Span>> {
        let t_start = Instant::now();
        let result = self.with_analysis(|a| {
            Some(a.with_def_names(name, |defs| {
                info!("defs: {:?}", defs);
                defs.into_iter()
                    .flat_map(|id| {
                        a.with_ref_spans(*id, |refs| {
                            def_span!(a, *id)
                                .into_iter()
                                .chain(refs.iter().cloned())
                                .collect::<Vec<_>>()
                        }).or_else(|| def_span!(a, *id).map(|s| vec![s]))
                            .unwrap_or_else(Vec::new)
                            .into_iter()
                    })
                    .collect(): Vec<Span>
            }))
        });

        let time = t_start.elapsed();
        info!(
            "search: {}s",
            time.as_secs() as f64 + time.subsec_nanos() as f64 / 1_000_000_000.0
        );
        result
    }

    // TODO refactor search and find_all_refs to use this
    // Includes all references and the def, the def is always first.
    pub fn find_all_refs_by_id(&self, id: Id) -> AResult<Vec<Span>> {
        let t_start = Instant::now();
        let result = self.with_analysis(|a| {
            a.with_ref_spans(id, |refs| {
                def_span!(a, id)
                    .into_iter()
                    .chain(refs.iter().cloned())
                    .collect::<Vec<_>>()
            }).or_else(|| def_span!(a, id).map(|s| vec![s]))
        });

        let time = t_start.elapsed();
        info!(
            "find_all_refs_by_id: {}s",
            time.as_secs() as f64 + time.subsec_nanos() as f64 / 1_000_000_000.0
        );
        result
    }

    pub fn find_impls(&self, id: Id) -> AResult<Vec<Span>> {
        self.with_analysis(|a| {
            Some(a.for_all_crates(|c| c.impls.get(&id).map(|v| v.clone())))
        })
    }

    /// Search for a symbol name, returning a list of def_ids for that name.
    pub fn search_for_id(&self, name: &str) -> AResult<Vec<Id>> {
        self.with_analysis(|a| Some(a.with_def_names(name, |defs| defs.clone())))
    }

    pub fn symbols(&self, file_name: &Path) -> AResult<Vec<SymbolResult>> {
        self.with_analysis(|a| {
            a.with_defs_per_file(file_name, |ids| {
                ids.iter()
                    .map(|id| {
                        a.with_defs(*id, |def| SymbolResult::new(*id, def)).unwrap()
                    })
                    .collect()
            })
        })
    }

    pub fn doc_url(&self, span: &Span) -> AResult<String> {
        // e.g., https://doc.rust-lang.org/nightly/std/string/String.t.html
        self.with_analysis(|a| {
            a.def_id_for_span(span).and_then(|id| {
                a.with_defs_and_then(id, |def| AnalysisHost::<L>::mk_doc_url(def, a))
            })
        })
    }

    // e.g., https://github.com/rust-lang/rust/blob/master/src/liballoc/string.rs#L261-L263
    pub fn src_url(&self, span: &Span) -> AResult<String> {
        // FIXME would be nice not to do this every time.
        let path_prefix = &self.loader.abs_path_prefix();

        self.with_analysis(|a| {
            a.def_id_for_span(span).and_then(|id| {
                a.with_defs_and_then(id, |def| {
                    AnalysisHost::<L>::mk_src_url(def, path_prefix.as_ref(), a)
                })
            })
        })
    }

    fn with_analysis<F, T>(&self, f: F) -> AResult<T>
    where
        F: FnOnce(&Analysis) -> Option<T>,
    {
        let a = self.analysis.lock()?;
        if let Some(ref a) = *a {
            f(a).ok_or(AError::Unclassified)
        } else {
            Err(AError::Unclassified)
        }
    }

    fn mk_doc_url(def: &Def, analysis: &Analysis) -> Option<String> {
        if !def.distro_crate {
            return None;
        }

        if def.parent.is_none() && def.qualname.contains('<') {
            debug!(
                "mk_doc_url, bailing, found generic qualname: `{}`",
                def.qualname
            );
            return None;
        }

        match def.parent {
            Some(p) => {
                analysis.with_defs(p, |parent| match def.kind {
                    DefKind::Field | DefKind::Method | DefKind::Tuple => {
                        let ns = name_space_for_def_kind(def.kind);
                        let mut res = AnalysisHost::<L>::mk_doc_url(&parent, analysis)
                            .unwrap_or_else(|| "".into());
                        res.push_str(&format!("#{}.{}", def.name, ns));
                        res
                    }
                    DefKind::Mod => {
                        let parent_qualpath = parent.qualname.replace("::", "/");
                        format!(
                            "{}/{}/{}/",
                            analysis.doc_url_base,
                            parent_qualpath.trim_right_matches('/'),
                            def.name,
                        )
                    }
                    _ => {
                        let parent_qualpath = parent.qualname.replace("::", "/");
                        let ns = name_space_for_def_kind(def.kind);
                        format!(
                            "{}/{}/{}.{}.html",
                            analysis.doc_url_base,
                            parent_qualpath,
                            def.name,
                            ns,
                        )
                    }
                })
            }
            None => {
                let qualpath = def.qualname.replace("::", "/");
                let ns = name_space_for_def_kind(def.kind);
                Some(format!(
                    "{}/{}.{}.html",
                    analysis.doc_url_base,
                    qualpath,
                    ns,
                ))
            }
        }
    }

    fn mk_src_url(def: &Def, path_prefix: Option<&PathBuf>, analysis: &Analysis) -> Option<String> {
        if !def.distro_crate {
            return None;
        }

        let file_path = &def.span.file;
        let file_path = file_path.strip_prefix(path_prefix?).ok()?;

        Some(format!(
            "{}/{}#L{}-L{}",
            analysis.src_url_base,
            file_path.to_str().unwrap(),
            def.span.range.row_start.one_indexed().0,
            def.span.range.row_end.one_indexed().0
        ))
    }
}

impl ::std::fmt::Display for Id {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        self.0.fmt(f)
    }
}

impl ::std::error::Error for AError {
    fn description(&self) -> &str {
        match *self {
            AError::MutexPoison => "poison error in a mutex (usually a secondary error)",
            AError::Unclassified => "unknown error",
        }
    }
}

impl ::std::fmt::Display for AError {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {
        write!(f, "{}", ::std::error::Error::description(self))
    }
}

impl<T> From<::std::sync::PoisonError<T>> for AError {
    fn from(_: ::std::sync::PoisonError<T>) -> AError {
        AError::MutexPoison
    }
}
