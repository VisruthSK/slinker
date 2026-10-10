use super::client::WorkerClient;
use super::protocol::{
    DataLibraryFiles, NamespaceImageSpec, PackageSpec, PayloadSerialization, PayloadSpec,
    RelocationSiteSpec,
};
use crate::package::inspection::Lanes;
use crate::package::{CanonicalSyntax, DataSetId, DatasetName, FrozenPackages, SyntaxValidation};
use crate::{Result, TargetEnvironment, WorkerExecutable};
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug)]
pub(crate) struct WorkerService {
    packages: Arc<FrozenPackages>,
    executable: WorkerExecutable,
    inspection: Arc<Lanes>,
}

impl WorkerService {
    pub(crate) fn new(
        target: TargetEnvironment,
        executable: WorkerExecutable,
        count: usize,
    ) -> Arc<Self> {
        Self::from_packages(Arc::new(FrozenPackages::new(target)), executable, count)
    }

    fn from_packages(
        packages: Arc<FrozenPackages>,
        executable: WorkerExecutable,
        count: usize,
    ) -> Arc<Self> {
        let inspection = Arc::new(Lanes::new(
            Arc::clone(&packages),
            executable.clone(),
            count.clamp(1, 2),
        ));
        Arc::new(Self {
            packages,
            executable,
            inspection,
        })
    }

    pub(crate) fn primed(count: usize, client: WorkerClient) -> Arc<Self> {
        let service = Self::from_packages(client.packages(), client.executable(), count);
        service.inspection.prime(client);
        service
    }

    pub(crate) fn target(&self) -> &TargetEnvironment {
        self.packages.target()
    }

    pub(crate) fn packages(&self) -> Arc<FrozenPackages> {
        Arc::clone(&self.packages)
    }

    pub(crate) fn check(&self) -> Result<()> {
        self.packages.check()
    }

    pub(crate) fn inspection(&self) -> Arc<Lanes> {
        Arc::clone(&self.inspection)
    }

    pub(crate) fn preparation(&self) -> Result<PreparationWorker> {
        Ok(PreparationWorker(WorkerClient::spawn(
            Arc::clone(&self.packages),
            0,
            &self.executable,
        )?))
    }
}

// Preparation can mutate package images; its client cannot enter the inspection pool.
pub(crate) struct PreparationWorker(WorkerClient);

impl PreparationWorker {
    pub(crate) fn canonical_syntax(&mut self, source: &str) -> Result<CanonicalSyntax> {
        self.0.canonical_syntax(source)
    }
    pub(crate) fn validate_syntax(&mut self, source: &str) -> Result<SyntaxValidation> {
        self.0.validate_syntax(source)
    }
    pub(crate) fn verify_relocation(
        &mut self,
        original: &str,
        rewritten: &str,
        sites: Vec<RelocationSiteSpec>,
    ) -> Result<SyntaxValidation> {
        self.0.verify_relocation(original, rewritten, sites)
    }
    pub(crate) fn serialize_payloads(
        &mut self,
        namespaces: Vec<NamespaceImageSpec>,
        payloads: Vec<PayloadSpec>,
    ) -> Result<PayloadSerialization> {
        self.0.serialize_payloads(namespaces, payloads)
    }
    pub(crate) fn data_library(
        &mut self,
        package: PackageSpec,
        objects: Vec<DatasetName>,
        sets: BTreeMap<DataSetId, Vec<DatasetName>>,
    ) -> Result<DataLibraryFiles> {
        self.0.data_library(package, objects, sets)
    }
}
