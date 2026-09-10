mod image;
mod index;
mod locator;
mod store;

pub use image::{
    BindingImage, BindingOrigin, ClosureSource, ObjectIssue, ObjectKind, PackageImage,
};
pub use index::{
    ExportMap, ImportBinding, ImportSpec, LifecycleMetadata, NativeComponent, PackageIndex,
    ResourceInfo, S3Registration,
};
pub use locator::{Digest, InstalledPackage, PackageId, PackageLocator};
pub use store::{PackageProvider, PackageStore, SyntaxValidation};

pub type BindingName = String;
pub type BindingKey = (PackageId, BindingName);
