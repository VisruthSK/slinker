namespaces <- new.env(hash = TRUE, parent = emptyenv())
.slinker_unregister <- function() {
  for (key in names(namespaces)) {
    if (identical(.Internal(getRegisteredNamespace(key)), namespaces[[key]])) {
      .Internal(unregisterNamespace(key))
    }
  }
}
.slinker_check_target <- function() {
  actual <- c(
    version = paste0(R.version$major, ".", R.version$minor),
    platform = R.version$os,
    arch = R.version$arch
  )
  if (!identical(unname(actual), unname(.slinker_target))) {
    stop(
      sprintf(
        "slinker target mismatch: expected %s, got %s",
        paste(.slinker_target, collapse = "/"),
        paste(actual, collapse = "/")
      ),
      call. = FALSE
    )
  }
}
.slinker_new_namespace <- function(key, name, version) {
  imports <- new.env(parent = .BaseNamespaceEnv, hash = TRUE)
  attr(imports, "name") <- paste0("imports:", name)
  namespace <- new.env(parent = imports, hash = TRUE)
  info <- new.env(hash = TRUE, parent = baseenv())
  namespace$.__NAMESPACE__. <- info
  namespace$.packageName <- name
  info$spec <- c(name = name, version = version)
  setNamespaceInfo(
    namespace,
    "exports",
    new.env(hash = TRUE, parent = baseenv())
  )
  setNamespaceInfo(namespace, "imports", list(base = TRUE))
  setNamespaceInfo(namespace, "path", "")
  setNamespaceInfo(namespace, "dynlibs", character())
  setNamespaceInfo(namespace, "DLLs", list())
  lazydata <- new.env(hash = TRUE, parent = baseenv())
  attr(lazydata, "name") <- paste0("lazydata:", name)
  setNamespaceInfo(namespace, "lazydata", lazydata)
  setNamespaceInfo(namespace, "S3methods", matrix(NA_character_, 0L, 4L))
  namespace$.__S3MethodsTable__. <- new.env(hash = TRUE, parent = baseenv())
  .Internal(registerNamespace(key, namespace))
  namespace
}
.slinker_lazydata <- function(namespace, package) {
  directory <- system.file(
    "slinker",
    "datalib",
    package,
    "data",
    package = .slinker_root_package,
    mustWork = TRUE
  )
  lazyLoad(
    file.path(directory, "Rdata"),
    envir = getNamespaceInfo(namespace, "lazydata")
  )
}
.slinker_load_native <- function(
  namespace,
  package,
  component,
  alias,
  library,
  symbols
) {
  path <- system.file(
    "slinker",
    "resources",
    package,
    library,
    package = .slinker_root_package,
    mustWork = TRUE
  )
  dll <- dyn.load(path, local = TRUE)
  dlls <- getNamespaceInfo(namespace, "DLLs")
  dlls[[component]] <- dll
  setNamespaceInfo(namespace, "DLLs", dlls)
  setNamespaceInfo(
    namespace,
    "dynlibs",
    c(
      getNamespaceInfo(namespace, "dynlibs"),
      structure(component, names = alias)
    )
  )
  for (binding in names(symbols)) {
    assign(
      binding,
      getNativeSymbolInfo(symbols[[binding]], dll),
      envir = namespace
    )
  }
}
.slinker_populate <- function(namespace, package, external) {
  for (dependency in external) {
    loadNamespace(dependency)
  }
  bundle <- system.file(
    "slinker",
    "payload",
    paste0(package, ".rds"),
    package = .slinker_root_package,
    mustWork = TRUE
  )
  invisible(list2env(readRDS(bundle), envir = namespace))
}
.slinker_stub <- function(envir, name, package, binding) {
  force(package)
  force(binding)
  makeActiveBinding(
    name,
    function(value) {
      stop(
        sprintf(
          "`%s::%s` was removed by slinker because the build never reached it",
          package,
          binding
        ),
        call. = FALSE
      )
    },
    envir
  )
}
.slinker_register_s3 <- function(namespace, s3, s3_info) {
  previous <- getNamespaceInfo(namespace, "S3methods")
  registerS3methods(s3, unname(getNamespaceName(namespace)), namespace)
  setNamespaceInfo(namespace, "S3methods", rbind(s3_info, previous))
}
.slinker_activate <- function(
  namespace,
  exports,
  s3,
  s3_info,
  removed,
  on_load
) {
  name <- unname(getNamespaceName(namespace))
  if (nrow(s3)) {
    .slinker_register_s3(namespace, s3, s3_info)
  }
  if (on_load) {
    get(".onLoad", envir = namespace, inherits = FALSE)("", name)
  }
  for (binding in removed[
    !vapply(removed, exists, logical(1), envir = namespace, inherits = FALSE)
  ]) {
    .slinker_stub(namespace, binding, name, binding)
  }
  if (length(exports)) {
    namespaceExport(namespace, exports)
  }
  lockEnvironment(namespace, TRUE)
  lockEnvironment(parent.env(namespace), TRUE)
  invisible(namespace)
}
