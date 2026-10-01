mod index;
mod payload;

mod runtime;
mod scan;
mod serve;
#[cfg(test)]
mod tests;

pub use serve::run;

use harp::RObjectExt;
use protocol::WorkerErrorCode;
use slinker_core::worker::protocol;

#[derive(Debug, thiserror::Error)]
pub(crate) enum InspectionError {
    #[error(transparent)]
    R(Box<harp::Error>),
    #[error("{0}")]
    Failed(String),
}

impl From<harp::Error> for InspectionError {
    fn from(error: harp::Error) -> Self {
        Self::R(Box::new(error))
    }
}

impl From<String> for InspectionError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

type InspectionResult<T> = Result<T, InspectionError>;
type OperationResult<T> = Result<T, WorkerOperationError>;

#[derive(Debug)]
struct WorkerOperationError {
    code: WorkerErrorCode,
    error: InspectionError,
}

impl WorkerOperationError {
    fn with(code: WorkerErrorCode) -> impl FnOnce(InspectionError) -> Self {
        move |error| Self { code, error }
    }
}

fn field(object: &harp::object::RObject, name: &str) -> InspectionResult<harp::object::RObject> {
    Ok(object.elt(name)?)
}

fn string_field(object: &harp::object::RObject, name: &str) -> InspectionResult<String> {
    Ok(String::try_from(field(object, name)?)?)
}

fn strings_field(object: &harp::object::RObject, name: &str) -> InspectionResult<Vec<String>> {
    Ok(Vec::<String>::try_from(field(object, name)?)?)
}

fn list_field(
    object: &harp::object::RObject,
    name: &str,
) -> InspectionResult<Vec<harp::object::RObject>> {
    Ok(Vec::<harp::object::RObject>::try_from(field(
        object, name,
    )?)?)
}
