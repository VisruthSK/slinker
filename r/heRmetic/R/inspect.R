.hrm_inspect_object <- function(x, image_env, package, path = "$", depth = 0L) {
  issue <- function(path, kind, detail = "") list(path = path, kind = kind, detail = detail)
  env_record <- function(path, kind, name = "") list(path = path, kind = kind, name = name)

  issues <- list()
  envs <- list()
  add_issue <- function(value) issues[[length(issues) + 1L]] <<- value
  add_env <- function(value) envs[[length(envs) + 1L]] <<- value
  merge_child <- function(child) {
    if (length(child$issues)) issues <<- c(issues, child$issues)
    if (length(child$envs)) envs <<- c(envs, child$envs)
  }

  if (depth > 128L) {
    add_issue(issue(path, "object_depth", "object graph exceeds 128 levels"))
    return(list(type = typeof(x), issues = issues, envs = envs))
  }

  closure_environment <- function(env, path) {
    if (identical(env, image_env)) return(list(issue = NULL, env = env_record(path, "namespace", package)))
    if (isNamespace(env)) return(list(issue = NULL, env = env_record(path, "namespace", getNamespaceName(env))))
    if (identical(env, baseenv())) return(list(issue = NULL, env = env_record(path, "base", "base")))
    if (identical(env, .GlobalEnv)) {
      return(list(issue = issue(path, "environment_identity", ".GlobalEnv"), env = env_record(path, "global", ".GlobalEnv")))
    }
    name <- environmentName(env)
    list(
      issue = issue(path, "environment_identity", if (nzchar(name)) name else "local environment"),
      env = env_record(path, "local", name)
    )
  }

  type <- typeof(x)
  if (isS4(x)) add_issue(issue(path, "s4", paste(class(x), collapse = "/")))
  classes <- class(x)
  if (length(classes) && any(grepl("^S7", classes))) add_issue(issue(path, "s7", paste(classes, collapse = "/")))

  if (type == "environment") {
    add_issue(issue(path, "environment", "embedded environment"))
  } else if (type == "externalptr") {
    add_issue(issue(path, "external_pointer", "external pointer"))
  } else if (type == "weakref") {
    add_issue(issue(path, "weak_reference", "weak reference"))
  } else if (type == "closure") {
    env_result <- closure_environment(environment(x), paste0(path, ".environment"))
    add_env(env_result$env)
    if (!is.null(env_result$issue)) add_issue(env_result$issue)
    merge_child(.hrm_inspect_object(formals(x), image_env, package, paste0(path, ".formals"), depth + 1L))
    merge_child(.hrm_inspect_object(body(x), image_env, package, paste0(path, ".body"), depth + 1L))
  } else if (type %in% c("list", "expression", "pairlist", "language")) {
    values <- as.list(x)
    if (length(values)) {
      for (i in seq_along(values)) {
        if (!.hrm_is_missing_slot(values, i)) {
          merge_child(.hrm_inspect_object(values[[i]], image_env, package, paste0(path, "[[", i, "]]"), depth + 1L))
        }
      }
    }
  } else if (!(type %in% c("NULL", "logical", "integer", "double", "complex", "character", "raw", "symbol", "builtin", "special"))) {
    add_issue(issue(path, "unsupported_type", type))
  }

  attrs <- attributes(x)
  if (length(attrs)) {
    for (name in names(attrs)) {
      merge_child(.hrm_inspect_object(attrs[[name]], image_env, package, paste0(path, ".attr[", name, "]"), depth + 1L))
    }
  }

  list(type = type, issues = issues, envs = envs)
}

.hrm_analysis_binding <- function(name, value) {
  lhs <- .hrm_quote_binding(name)
  rhs <- if (typeof(value) == "closure") {
    paste(deparse(value, width.cutoff = 500L, control = c("keepInteger", "keepNA", "niceNames")), collapse = "\n")
  } else {
    "NULL"
  }
  paste0(lhs, " <- ", rhs)
}

.hrm_inspect_job <- function(job) {
  library <- .hrm_normalize_library(job$library)
  package <- job$package
  output <- job$output
  recipes_output <- job$recipes_output
  analysis_dir <- job$analysis_dir
  visible_libraries <- .hrm_dedupe_libraries(c(library, job$visible_libraries, .Library))
  .libPaths(visible_libraries)

  if (file.exists(output)) invisible(file.remove(output))
  if (dir.exists(analysis_dir)) unlink(analysis_dir, recursive = TRUE, force = TRUE)
  dir.create(file.path(analysis_dir, "R"), recursive = TRUE, showWarnings = FALSE)
  invisible(file.create(file.path(analysis_dir, ".hrm-installed-image")))

  pkgpath <- file.path(library, package)
  if (!dir.exists(pkgpath)) stop(sprintf("installed package not found: %s", pkgpath), call. = FALSE)
  pkgpath <- normalizePath(pkgpath, winslash = "/", mustWork = TRUE)

  description_path <- file.path(pkgpath, "DESCRIPTION")
  namespace_path <- file.path(pkgpath, "NAMESPACE")
  ns_info_path <- file.path(pkgpath, "Meta", "nsInfo.rds")
  package_rds_path <- file.path(pkgpath, "Meta", "package.rds")
  for (path in c(description_path, ns_info_path, package_rds_path)) {
    if (!file.exists(path)) stop(sprintf("installed package metadata missing: %s", path), call. = FALSE)
  }

  invisible(file.copy(description_path, file.path(analysis_dir, "DESCRIPTION"), overwrite = TRUE))
  if (file.exists(namespace_path)) {
    invisible(file.copy(namespace_path, file.path(analysis_dir, "NAMESPACE"), overwrite = TRUE))
  } else {
    invisible(file.create(file.path(analysis_dir, "NAMESPACE")))
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
  all_names <- ls(image_env, all.names = TRUE)
  analysis_lines <- rep(NA_character_, length(all_names))

  recipe_env_ref <- function(env) {
    if (identical(env, image_env)) return(paste0("namespace:", package))
    if (isNamespace(env)) return(paste0("namespace:", getNamespaceName(env)))
    if (identical(env, baseenv())) return("base:base")
    stop("unsupported closure environment reached recipe generation", call. = FALSE)
  }

  emit <- function(kind, ...) .hrm_emit(output, kind, ...)
  emit_object <- function(kind, name, value, origin = NULL) {
    result <- .hrm_inspect_object(value, image_env, package)
    supported <- !length(result$issues)
    if (kind == "BINDING") emit(kind, name, origin, result$type, if (supported) "1" else "0")
    else emit(kind, name, result$type, if (supported) "1" else "0")
    for (x in result$issues) emit("ISSUE", x$path, x$kind, x$detail)
    for (x in result$envs) emit("CLOSURE_ENV", x$path, x$kind, x$name)
    list(
      supported = supported,
      recipe = if (supported) .hrm_scrub_recipe(value, recipe_env_ref) else NULL
    )
  }

  recipes <- list(bindings = list(), datasets = list())
  has_on_load <- ".onLoad" %in% all_names
  emit("HEADER", package, version, if (has_on_load) "1" else "0")

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

  for (i in seq_along(all_names)) {
    name <- all_names[[i]]
    value <- tryCatch(get(name, envir = image_env, inherits = FALSE), error = identity)
    origin <- if (name %in% sysdata_names) "sysdata" else "code"
    if (inherits(value, "error")) {
      emit("BINDING", name, origin, "unavailable", "0")
      emit("ISSUE", "$", "force_error", conditionMessage(value))
      next
    }
    analysis_lines[[i]] <- .hrm_analysis_binding(name, value)
    result <- emit_object("BINDING", name, value, origin)
    if (result$supported) recipes$bindings[name] <- list(result$recipe)
  }
  writeLines(analysis_lines[!is.na(analysis_lines)], file.path(analysis_dir, "R", "installed-image.R"), useBytes = TRUE)

  data_env <- new.env(hash = TRUE, parent = emptyenv())
  data_base <- file.path(pkgpath, "data", "Rdata")
  if (file.exists(paste0(data_base, ".rdx")) && file.exists(paste0(data_base, ".rdb"))) {
    base::lazyLoad(data_base, envir = data_env)
  }
  for (name in sort(ls(data_env, all.names = TRUE))) {
    value <- tryCatch(get(name, envir = data_env, inherits = FALSE), error = identity)
    if (inherits(value, "error")) {
      emit("DATASET", name, "unavailable", "0")
      emit("ISSUE", "$", "force_error", conditionMessage(value))
    } else {
      result <- emit_object("DATASET", name, value)
      if (result$supported) recipes$datasets[name] <- list(result$recipe)
    }
  }

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

  saveRDS(recipes, recipes_output, version = 3L)
  invisible(NULL)
}

hrm_inspect_batch <- function(manifest, jobs = 1L) {
  spec <- .hrm_read_inspection_manifest(manifest)
  if (!length(spec$jobs)) return(invisible(NULL))
  work <- lapply(spec$jobs, function(job) {
    job$visible_libraries <- spec$libraries
    job
  })
  invisible(.hrm_parallel_map(work, ".hrm_inspect_job", jobs))
}
