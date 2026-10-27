use crate::InspectionResult;
use harp::{RFunctionExt, RObjectExt};
use slinker_core::package::{DispatchObservation, GenericName};

pub(super) fn observation(value: &harp::RObject) -> InspectionResult<DispatchObservation> {
    if bool::try_from(value.elt("known")?)? {
        Ok(DispatchObservation::from_generics(
            Vec::<String>::try_from(&value.elt("generics")?)?
                .into_iter()
                .map(GenericName::from),
        ))
    } else {
        Ok(DispatchObservation::UnknownDispatch)
    }
}

pub(super) fn primitive(value: libr::SEXP) -> InspectionResult<DispatchObservation> {
    let result = harp::RFunction::new("", ".slinker_callable_dispatch")
        .add(value)
        .call()?;
    observation(&result)
}
