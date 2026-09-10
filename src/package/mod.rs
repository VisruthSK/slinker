mod image;
mod index;
mod locator;
mod store;

pub use image::{BindingImage, BindingOrigin, ClosureSource, EmbeddedClosureSource, ObjectIssue, ObjectKind, PackageImage};
pub use index::{
    ExportMap, ImportBinding, ImportSpec, LifecycleMetadata, NativeComponent, NativeFacts, NativeSafety, PackageIndex,
    S3Registration,
};
pub use locator::{Digest, InstalledPackage, PackageId, PackageLocator};
pub use store::{PackageProvider, PackageStore, SyntaxValidation};

pub type BindingName = String;
pub type BindingKey = (PackageId, BindingName);
