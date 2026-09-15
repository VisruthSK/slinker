.slinker_package_context <- function(root, package) {
  root <- normalizePath(root, winslash = "/", mustWork = TRUE)
  required <- file.path(root, c("DESCRIPTION", "Meta/nsInfo.rds", "Meta/package.rds"))
  if (any(!file.exists(required))) {
    stop(sprintf("installed package metadata missing under %s", root), call. = FALSE)
  }

  image_env <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
  code_db <- file.path(root, "R", package)
  if (!file.exists(paste0(code_db, ".rdx")) || !file.exists(paste0(code_db, ".rdb"))) {
    stop(sprintf("installed R lazy-load database missing for %s", package), call. = FALSE)
  }
  base::lazyLoad(code_db, envir = image_env)

  before <- ls(image_env, all.names = TRUE)
  sysdata_db <- file.path(root, "R", "sysdata")
  if (file.exists(paste0(sysdata_db, ".rdx")) && file.exists(paste0(sysdata_db, ".rdb"))) {
    base::lazyLoad(sysdata_db, envir = image_env)
    sysdata_names <- setdiff(ls(image_env, all.names = TRUE), before)
  } else {
    sysdata_names <- character()
  }

  data_env <- new.env(hash = TRUE, parent = emptyenv())
  data_db <- file.path(root, "data", "Rdata")
  if (file.exists(paste0(data_db, ".rdx")) && file.exists(paste0(data_db, ".rdb"))) {
    base::lazyLoad(data_db, envir = data_env)
  }

  list(
    package = package,
    version = unname(readRDS(file.path(root, "Meta", "package.rds"))$DESCRIPTION[["Version"]]),
    ns_info = readRDS(file.path(root, "Meta", "nsInfo.rds")),
    image_env = image_env,
    binding_names = sort(setdiff(
      ls(image_env, all.names = TRUE),
      c(".__NAMESPACE__.", ".__S3MethodsTable__.", ".packageName")
    )),
    sysdata_names = sort(sysdata_names),
    dataset_names = sort(ls(data_env, all.names = TRUE))
  )
}

.slinker_package_metadata <- function(context) {
  ns <- context$ns_info
  exports <- as.character(ns$exports)
  export_names <- names(ns$exports)
  if (is.null(export_names)) export_names <- exports
  export_names[!nzchar(export_names)] <- exports[!nzchar(export_names)]
  for (pattern in ns$exportPatterns) {
    matches <- setdiff(
      ls(context$image_env, pattern = pattern, all.names = TRUE),
      c(".__NAMESPACE__.", ".__S3MethodsTable__.", ".packageName")
    )
    exports <- c(exports, matches)
    export_names <- c(export_names, matches)
  }
  unique_exports <- !duplicated(paste0(export_names, "\r", exports))

  imports <- lapply(ns$imports, function(entry) {
    if (is.character(entry)) {
      return(list(kind = "all", package = entry, except = character()))
    }
    if (!is.null(entry$except)) {
      return(list(kind = "all", package = as.character(entry[[1L]]), except = as.character(entry$except)))
    }
    remote <- as.character(entry[[2L]])
    local <- names(entry[[2L]])
    if (is.null(local)) local <- remote
    local[!nzchar(local)] <- remote[!nzchar(local)]
    list(kind = "from", package = as.character(entry[[1L]]), remote = remote, local = local)
  })

  s3 <- ns$S3methods
  s3 <- if (length(s3)) {
    s3 <- as.matrix(s3)
    lapply(seq_len(nrow(s3)), function(i) list(
      generic = s3[i, 1L],
      generic_package = if (ncol(s3) >= 4L && !is.na(s3[i, 4L])) s3[i, 4L] else character(),
      class = s3[i, 2L],
      method = if (ncol(s3) >= 3L && !is.na(s3[i, 3L])) s3[i, 3L] else paste(s3[i, 1L], s3[i, 2L], sep = ".")
    ))
  } else list()

  dynlibs <- lapply(as.character(ns$dynlibs), function(dll) {
    native <- ns$nativeRoutines[[dll]]
    registered <- !is.null(native) && isTRUE(native$useRegistration)
    fixes <- if (registered && length(native$registrationFixes) >= 2L) {
      as.character(native$registrationFixes[1:2])
    } else c("", "")
    symbols <- if (is.null(native)) character() else native$symbolNames
    bindings <- names(symbols)
    if (is.null(bindings)) bindings <- as.character(symbols)
    list(
      name = dll,
      registered = registered,
      prefix = fixes[[1L]],
      suffix = fixes[[2L]],
      bindings = bindings,
      symbols = as.character(symbols)
    )
  })

  list(
    name = context$package,
    version = context$version,
    export_names = export_names[unique_exports],
    export_bindings = exports[unique_exports],
    imports = imports,
    s3 = s3,
    dynlibs = dynlibs,
    on_load = ".onLoad" %in% context$binding_names,
    binding_names = context$binding_names,
    datasets = context$dataset_names,
    has_sysdata = length(context$sysdata_names) > 0L
  )
}

.slinker_deparse_binding <- function(name, value, embedded = FALSE) {
  rhs <- paste(deparse(
    value,
    width.cutoff = 500L,
    control = c("keepInteger", "keepNA", "niceNames")
  ), collapse = "\n")
  if (embedded) paste0(".slinker_embedded <- ", rhs) else {
    simple <- grepl("^[A-Za-z.][A-Za-z0-9._]*$", name) && !grepl("^\\.[0-9]", name)
    lhs <- if (simple) name else paste0("`", gsub("`", "\\\\`", name, fixed = TRUE), "`")
    paste0(lhs, " <- ", rhs)
  }
}
