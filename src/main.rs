extern crate rls_analysis;
extern crate env_logger;

use std::path::{Path, PathBuf};

use rls_analysis::{AnalysisLoader, AnalysisHost};

#[derive(Clone)]
struct TestAnalysisLoader {
    paths: Vec<PathBuf>,
}

impl TestAnalysisLoader {
    fn new(paths: Vec<PathBuf>) -> Self {
        Self {
            paths
        }
    }
}

impl AnalysisLoader for TestAnalysisLoader {
    fn needs_hard_reload(&self, _path_prefix: &Path) -> bool {
        true
    }

    fn fresh_host(&self) -> AnalysisHost<Self> {
        AnalysisHost::new_with_loader(self.clone())
    }

    fn set_path_prefix(&self, _path_prefix: &Path) {}

    fn abs_path_prefix(&self) -> Option<PathBuf> {
        panic!();
    }

    fn iter_paths<F, T>(&self, f: F) -> Vec<T>
    where
        F: Fn(&Path) -> Vec<T>,
    {
        let paths = &self.paths;
        paths.iter().flat_map(|p| f(p).into_iter()).collect()
    }
}

fn main() {
    let _ = env_logger::init().unwrap();

    let paths = PathBuf::from("/home/xanewok/repos/different_dep_versions/target/debug/deps/save-analysis");
    let loader = TestAnalysisLoader::new(vec![paths]);

    //let path_prefix = PathBuf::from("/home/xanewok/repos/different_dep_versions");
    let host = AnalysisHost::new_with_loader(loader);
    host.reload(
        Path::new("/home/xanewok/repos/different_dep_versions"),
        Path::new("/home/xanewok/repos/different_dep_versions"),
    ).unwrap();
    let _ = host.search("KURWA");
    // let host = AnalysisHost::new_with_loader(TestAnalysisLoader::new(
    //     Path::new("test_data/rls-analysis").to_owned(),
    // ));
    // host.reload(
    //     Path::new("test_data/rls-analysis"),
    //     Path::new("test_data/rls-analysis"),
    // ).unwrap();
}