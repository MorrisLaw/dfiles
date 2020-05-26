use dockworker;
use thiserror;
use which;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("not in entrypoint mode")]
    NotInEntrypointMode,

    #[error("missing entrypoint args")]
    MissingEntrypointArgs,

    #[error("could not find current binary")]
    CouldNotFindCurrentBinary(#[from] std::io::Error),

    #[error("could not find current binary")]
    FailedToAddFileToArchive { source: std::io::Error },

    #[error("could not identify user with uid `{0:?}`")]
    MissingUser(String),

    #[error("could not identify user with gid `{0:?}`")]
    MissingGroup(String),

    #[error("invalid mount string `{0:?}`")]
    InvalidMount(String),

    #[error("invalide locale `{0:?}`")]
    InvalidLocale(String),

    #[error("could not identify directory")]
    MissingDirectory,

    #[error("directory")]
    DockerError(#[from] dockworker::errors::Error),

    #[error("failed to save config to file")]
    FailedToSaveConfig,

    #[error("failed to load config from file")]
    FailedToLoadConfig,

    #[error("failed to find binary")]
    WhichError(#[from] which::Error),
}
