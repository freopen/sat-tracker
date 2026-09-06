#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuildInfo {
    pub version: &'static str,
    pub build_time: &'static str,
    pub git_commit: &'static str,
    pub git_dirty: &'static str,
}

pub const fn build_info() -> BuildInfo {
    BuildInfo {
        version: env!("CARGO_PKG_VERSION"),
        build_time: env!("VERGEN_BUILD_TIMESTAMP"),
        git_commit: match option_env!("VERGEN_GIT_SHA") {
            Some(value) => value,
            None => "unknown",
        },
        git_dirty: match option_env!("VERGEN_GIT_DIRTY") {
            Some(value) => value,
            None => "unknown",
        },
    }
}

impl BuildInfo {
    pub fn message(self) -> String {
        format!(
            "Version: {}\nBuild time: {}\nGit commit: {}\nGit dirty: {}",
            self.version, self.build_time, self.git_commit, self.git_dirty
        )
    }
}
