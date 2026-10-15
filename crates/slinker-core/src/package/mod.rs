mod cache_names;
mod cache_report;
mod identity;
mod image;
mod index;
pub(crate) mod inspection;
mod intern;
mod locator;
mod names;
mod resource;
pub(crate) mod store;
mod universe;

pub use cache_names::EntryKind;
pub use cache_report::{
    CacheReport, ClearOutcome, ClearScope, KindTotals, PackageCacheReport, SchemaCacheReport,
    clear_cache, inspect_cache,
};
pub use identity::{
    Digest, InstalledPackage, PackageId, PackageIdentity, PackageLocation, PackageRole,
};
pub use image::{
    BindingImage, BindingOrigin, BindingRepresentation, ClosureSource, EmbeddedClosureSource,
    EmbeddedEnvironmentRef, ObjectImage, ObjectIssue, ObjectIssueKind, ObjectKind, PackageImage,
    PrivateBindingImage, PrivateEnvironmentImage, UnsupportedObject,
};
pub use index::{
    BindingNames, DataStorage, ExportMap, GenericSpec, ImportBinding, ImportSpec,
    LifecycleMetadata, NameLookup, NativeComponent, NativeFacts, NativeInterface, NativeLibrary,
    NativeRegistration, NativeRoutineSummary, NativeRoutines, NativeSafety, NativeSymbolBinding,
    PackageData, PackageIndex, S3Registration,
};
pub(crate) use locator::FrozenPackages;
pub use locator::PackageLocator;
pub(crate) use locator::fingerprint_image;
pub use locator::{Fingerprint, tree_digest};
pub use resource::{InvalidResourcePath, ResourcePath};
pub use store::{
    CanonicalSyntax, DispatchSubject, Normalization, PackageProvider, PackageResolver,
    PackageStore, SyntaxValidation, analysis_schema,
};
pub use universe::{DispatchCallee, PackageAvailability, PackageSources, TargetUniverse};

pub use names::{
    Atom, BindingName, ClassName, ComponentName, DataSetId, DatasetName, EnvironmentKind,
    EnvironmentLabel, ExportName, GenericLabel, GenericName, MemberPath, PackageName, SymbolName,
};

pub(crate) use image::reachable_environment_labels;
