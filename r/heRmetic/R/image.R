.hrm_closure_environment <- function(env, image_env, package) {
  if (identical(env, image_env)) return(paste0("namespace:", package))
  if (isNamespace(env)) return(paste0("namespace:", getNamespaceName(env)))
  if (identical(env, baseenv())) return("base:base")
  if (identical(env, .GlobalEnv)) return("unsupported:global")
  name <- environmentName(env)
  paste0("unsupported:", if (nzchar(name)) name else "local")
}

hrm_inspect_image <- function(library, package, output, visible_libraries = character()) {
  library <- .hrm_normalize_library(library)
  .libPaths(.hrm_dedupe_libraries(c(library, visible_libraries, .Library)))

  pkgpath <- file.path(library, package)
  if (!dir.exists(pkgpath)) stop(sprintf("installed package not found: %s", pkgpath), call. = FALSE)
  pkgpath <- normalizePath(pkgpath, winslash = "/", mustWork = TRUE)

  description_path <- file.path(pkgpath, "DESCRIPTION")
  ns_info_path <- file.path(pkgpath, "Meta", "nsInfo.rds")
  package_rds_path <- file.path(pkgpath, "Meta", "package.rds")
  for (path in c(description_path, ns_info_path, package_rds_path)) {
    if (!file.exists(path)) stop(sprintf("installed package metadata missing: %s", path), call. = FALSE)
  }

  ns_info <- readRDS(ns_info_path)
  pkg_info <- readRDS(package_rds_path)
  version <- unname(pkg_info$DESCRIPTION[["Version"]])

  image_env <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
  code_db <- file.path(pkgpath, "R", package)
  if (!file.exists(paste0(code_db, ".rdx")) || !file.exists(paste0(code_db, ".rdb"))) {
    stop(sprintf("installed R lazy-load database missing for %s", package), call. = FALSE)
  }
  base::lazyLoad(code_db, envir = image_env)
  code_names <- ls(image_env, all.names = TRUE)

  sysdata_base <- file.path(pkgpath, "R", "sysdata")
  if (file.exists(paste0(sysdata_base, ".rdx")) && file.exists(paste0(sysdata_base, ".rdb"))) {
    before <- ls(image_env, all.names = TRUE)
    base::lazyLoad(sysdata_base, envir = image_env)
    sysdata_names <- setdiff(ls(image_env, all.names = TRUE), before)
  } else {
    sysdata_names <- character()
  }
  all_names <- sort(ls(image_env, all.names = TRUE))

  data_env <- new.env(hash = TRUE, parent = emptyenv())
  data_base <- file.path(pkgpath, "data", "Rdata")
  if (file.exists(paste0(data_base, ".rdx")) && file.exists(paste0(data_base, ".rdb"))) {
    base::lazyLoad(data_base, envir = data_env)
  }
  dataset_names <- sort(ls(data_env, all.names = TRUE))

  if (file.exists(output)) invisible(file.remove(output))
  connection <- file(output, open = "wt", encoding = "UTF-8")
  on.exit(close(connection), add = TRUE)
  emit <- function(kind, ...) .hrm_emit_connection(connection, kind, ...)

  emit("HEADER", package, version, if (".onLoad" %in% all_names) "1" else "0", if (length(sysdata_names)) "1" else "0")

  if (length(ns_info$importClasses)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_import_classes", "importClasses is outside the v0.1 object model")
  if (length(ns_info$importMethods)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_import_methods", "importMethods is outside the v0.1 object model")
  if (length(ns_info$exportClasses)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_export_classes", "exportClasses is outside the v0.1 object model")
  if (length(ns_info$exportMethods)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_export_methods", "exportMethods is outside the v0.1 object model")

  exports <- as.character(ns_info$exports)
  export_names <- names(ns_info$exports)
  if (is.null(export_names)) export_names <- exports
  empty_export_names <- !nzchar(export_names)
  export_names[empty_export_names] <- exports[empty_export_names]
  export_pairs <- Map(function(name, binding) c(name = name, binding = binding), export_names, exports)
  for (pattern in ns_info$exportPatterns) {
    matches <- ls(image_env, pattern = pattern, all.names = TRUE)
    export_pairs <- c(export_pairs, Map(function(name) c(name = name, binding = name), matches))
  }
  if (length(export_pairs)) {
    keys <- vapply(export_pairs, function(x) paste0(x[["name"]], "\r", x[["binding"]]), character(1L))
    export_pairs <- export_pairs[!duplicated(keys)]
    export_pairs <- export_pairs[order(vapply(export_pairs, `[[`, character(1L), "name"))]
    for (pair in export_pairs) emit("EXPORT", pair[["name"]], pair[["binding"]])
  }

  for (entry in ns_info$imports) {
    if (is.character(entry)) {
      emit("IMPORT_ALL", entry)
    } else if (!is.null(entry$except)) {
      from <- as.character(entry[[1L]])
      emit("IMPORT_ALL", from)
      for (name in as.character(entry$except)) emit("IMPORT_EXCEPT", from, name)
    } else {
      from <- as.character(entry[[1L]])
      vars <- as.character(entry[[2L]])
      local_names <- names(entry[[2L]])
      if (is.null(local_names)) local_names <- vars
      empty_names <- !nzchar(local_names)
      local_names[empty_names] <- vars[empty_names]
      for (i in seq_along(vars)) emit("IMPORT_FROM", from, vars[[i]], local_names[[i]])
    }
  }

  for (name in all_names) {
    origin <- if (name %in% sysdata_names) "sysdata" else "code"
    value <- tryCatch(get(name, envir = image_env, inherits = FALSE), error = identity)
    if (inherits(value, "error")) {
      emit("BINDING", name, origin, "unavailable")
      emit("BINDING_ISSUE", name, "$", "force_error", conditionMessage(value))
      next
    }
    type <- typeof(value)
    emit("BINDING", name, origin, type)
    if (identical(type, "closure")) {
      formals_text <- paste(deparse(formals(value), width.cutoff = 500L, control = c("keepInteger", "keepNA", "niceNames")), collapse = "\n")
      body_text <- paste(deparse(body(value), width.cutoff = 500L, control = c("keepInteger", "keepNA", "niceNames")), collapse = "\n")
      source <- .hrm_analysis_binding(name, value)
      environment_ref <- .hrm_closure_environment(environment(value), image_env, package)
      emit("CLOSURE", name, environment_ref, formals_text, body_text, source)
    }
  }

  for (name in dataset_names) emit("DATASET", name)

  s3 <- ns_info$S3methods
  if (length(s3)) {
    s3 <- as.matrix(s3)
    for (i in seq_len(nrow(s3))) {
      method <- if (ncol(s3) >= 3L && !is.na(s3[i, 3L])) s3[i, 3L] else paste(s3[i, 1L], s3[i, 2L], sep = ".")
      emit("S3", s3[i, 1L], s3[i, 2L], method)
    }
  }

  for (dll in as.character(ns_info$dynlibs)) emit("DYNLIB", dll)
  all_files <- list.files(pkgpath, recursive = TRUE, all.files = TRUE, full.names = FALSE, include.dirs = FALSE, no.. = TRUE)
  for (rel in sort(all_files)) emit("RESOURCE", chartr("\\", "/", rel))
  invisible(NULL)
}

.hrm_read_image_manifest <- function(path) {
  records <- strsplit(readLines(path, warn = FALSE), "\t", fixed = TRUE)
  libraries <- character()
  jobs <- list()

  for (record in records) {
    if (!length(record)) next
    kind <- record[[1L]]
    fields <- vapply(record[-1L], .hrm_unhex, character(1L), USE.NAMES = FALSE)
    if (identical(kind, "LIB")) {
      if (length(fields) != 1L) stop("invalid LIB record", call. = FALSE)
      libraries <- c(libraries, fields[[1L]])
    } else if (identical(kind, "JOB")) {
      if (length(fields) != 3L) stop("invalid image JOB record", call. = FALSE)
      jobs[[length(jobs) + 1L]] <- list(
        library = fields[[1L]],
        package = fields[[2L]],
        output = fields[[3L]]
      )
    } else {
      stop(sprintf("unknown image manifest record '%s'", kind), call. = FALSE)
    }
  }

  list(libraries = libraries, jobs = jobs)
}

.hrm_inspect_image_job <- function(job) {
  hrm_inspect_image(
    job$library,
    job$package,
    job$output,
    job$visible_libraries
  )
  invisible(job$output)
}

hrm_inspect_image_batch <- function(manifest, jobs = 1L) {
  spec <- .hrm_read_image_manifest(manifest)
  if (!length(spec$jobs)) return(invisible(NULL))
  work <- lapply(spec$jobs, function(job) {
    job$visible_libraries <- spec$libraries
    job
  })
  invisible(.hrm_parallel_map(work, ".hrm_inspect_image_job", jobs))
}
