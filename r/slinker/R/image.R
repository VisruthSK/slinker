.slinker_namespace_metadata <- c(".__NAMESPACE__.", ".__S3MethodsTable__.", ".packageName")

.slinker_closure_environment <- function(env, image_env, package) {
  if (identical(env, image_env)) return(paste0("namespace:", package))
  if (isNamespace(env)) return(paste0("namespace:", getNamespaceName(env)))
  if (identical(env, baseenv())) return("base:base")
  if (identical(env, .GlobalEnv)) return("unsupported:global")
  name <- environmentName(env)
  paste0("unsupported:", if (nzchar(name)) name else "local")
}

.slinker_package_image_context <- function(library, package, visible_libraries = character()) {
  library <- .slinker_normalize_library(library)
  .libPaths(.slinker_dedupe_libraries(c(library, visible_libraries, .Library)))

  pkgpath <- file.path(library, package)
  if (!dir.exists(pkgpath)) stop(sprintf("installed package not found: %s", pkgpath), call. = FALSE)
  pkgpath <- normalizePath(pkgpath, winslash = "/", mustWork = TRUE)

  ns_info_path <- file.path(pkgpath, "Meta", "nsInfo.rds")
  package_rds_path <- file.path(pkgpath, "Meta", "package.rds")
  for (path in c(file.path(pkgpath, "DESCRIPTION"), ns_info_path, package_rds_path)) {
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

  sysdata_base <- file.path(pkgpath, "R", "sysdata")
  if (file.exists(paste0(sysdata_base, ".rdx")) && file.exists(paste0(sysdata_base, ".rdb"))) {
    before <- ls(image_env, all.names = TRUE)
    base::lazyLoad(sysdata_base, envir = image_env)
    sysdata_names <- setdiff(ls(image_env, all.names = TRUE), before)
  } else {
    sysdata_names <- character()
  }

  all_names <- sort(setdiff(ls(image_env, all.names = TRUE), .slinker_namespace_metadata))

  data_env <- new.env(hash = TRUE, parent = emptyenv())
  data_base <- file.path(pkgpath, "data", "Rdata")
  if (file.exists(paste0(data_base, ".rdx")) && file.exists(paste0(data_base, ".rdb"))) {
    base::lazyLoad(data_base, envir = data_env)
  }

  list(
    package = package,
    version = version,
    path = pkgpath,
    ns_info = ns_info,
    image_env = image_env,
    binding_names = all_names,
    sysdata_names = sysdata_names,
    dataset_names = sort(ls(data_env, all.names = TRUE)),
    has_sysdata = length(sysdata_names) > 0L,
    on_load = ".onLoad" %in% all_names
  )
}

.slinker_emit_package_index <- function(context, emit, binding_record = "BINDING_NAME") {
  ns_info <- context$ns_info
  image_env <- context$image_env

  emit(
    "HEADER",
    context$package,
    context$version,
    if (context$on_load) "1" else "0",
    if (context$has_sysdata) "1" else "0"
  )

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
    matches <- setdiff(ls(image_env, pattern = pattern, all.names = TRUE), .slinker_namespace_metadata)
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

  if (identical(binding_record, "BINDING_NAME")) {
    for (name in context$binding_names) emit("BINDING_NAME", name)
  }
  for (name in context$dataset_names) emit("DATASET", name)

  s3 <- ns_info$S3methods
  if (length(s3)) {
    s3 <- as.matrix(s3)
    for (i in seq_len(nrow(s3))) {
      method <- if (ncol(s3) >= 3L && !is.na(s3[i, 3L])) s3[i, 3L] else paste(s3[i, 1L], s3[i, 2L], sep = ".")
      emit("S3", s3[i, 1L], s3[i, 2L], method)
    }
  }

  for (dll in as.character(ns_info$dynlibs)) {
    native <- ns_info$nativeRoutines[[dll]]
    use_registration <- !is.null(native) && isTRUE(native$useRegistration)
    fixes <- if (use_registration && length(native$registrationFixes) >= 2L) {
      as.character(native$registrationFixes[1:2])
    } else {
      c("", "")
    }
    emit(
      "DYNLIB",
      dll,
      if (use_registration) "1" else "0",
      fixes[[1L]],
      fixes[[2L]]
    )
    symbols <- if (is.null(native)) character() else native$symbolNames
    if (length(symbols)) {
      variables <- names(symbols)
      if (is.null(variables)) variables <- as.character(symbols)
      for (i in seq_along(symbols)) {
        emit("NATIVE_SYMBOL", dll, variables[[i]], as.character(symbols[[i]]))
      }
    }
  }

}

slinker_inspect_index <- function(library, package, output, visible_libraries = character()) {
  context <- .slinker_package_image_context(library, package, visible_libraries)
  if (file.exists(output)) invisible(file.remove(output))
  connection <- file(output, open = "wt", encoding = "UTF-8")
  on.exit(close(connection), add = TRUE)
  emit <- function(kind, ...) .slinker_emit_connection(connection, kind, ...)
  .slinker_emit_package_index(context, emit)
  invisible(NULL)
}

.slinker_new_scan_state <- function(image_env, package) {
  state <- new.env(parent = emptyenv())
  state$image_env <- image_env
  state$package <- package
  state$private_envs <- list()
  state$private_records <- list()
  state$private_status <- integer()
  state
}

.slinker_private_environment_id <- function(state, env) {
  if (length(state$private_envs)) {
    matches <- vapply(state$private_envs, identical, logical(1L), y = env)
    if (any(matches)) return(which(matches)[[1L]])
  }
  state$private_envs[[length(state$private_envs) + 1L]] <- env
  state$private_records[[length(state$private_envs)]] <- list(parent = NULL, bindings = list())
  state$private_status[[length(state$private_envs)]] <- 0L
  length(state$private_envs)
}

.slinker_environment_ref <- function(state, env) {
  if (identical(env, state$image_env)) return(paste0("namespace:", state$package))
  if (isNamespace(env)) return(paste0("namespace:", getNamespaceName(env)))
  if (identical(env, baseenv())) return("base:base")
  if (identical(env, emptyenv())) return("base:empty")
  if (identical(env, .GlobalEnv)) return("unsupported:global")
  name <- environmentName(env)
  if (grepl("^package:", name)) return(paste0("unsupported:", name))

  id <- .slinker_private_environment_id(state, env)
  .slinker_inventory_private_environment(state, env, id)
  paste0("private:", id)
}

.slinker_read_binding_without_firing <- function(env, name) {
  active <- tryCatch(bindingIsActive(name, env), error = function(...) FALSE)
  value <- if (isTRUE(active)) {
    tryCatch(activeBindingFunction(name, env), error = identity)
  } else {
    tryCatch(get(name, envir = env, inherits = FALSE), error = identity)
  }
  list(active = isTRUE(active), value = value)
}

.slinker_inventory_private_environment <- function(state, env, id) {
  status <- state$private_status[[id]]
  if (!identical(status, 0L)) return(invisible(NULL))
  state$private_status[[id]] <- 1L

  parent_ref <- .slinker_environment_ref(state, parent.env(env))
  record <- state$private_records[[id]]
  record$parent <- parent_ref
  state$private_records[[id]] <- record

  for (name in sort(ls(env, all.names = TRUE))) {
    read <- .slinker_read_binding_without_firing(env, name)
    if (inherits(read$value, "error")) {
      binding <- list(
        name = name,
        active = read$active,
        type = "unavailable",
        closure = NULL,
        environment = NULL,
        closures = list(),
        environments = list(),
        issues = list(list(path = "$", kind = "force_error", detail = conditionMessage(read$value)))
      )
    } else {
      value <- read$value
      scan <- .slinker_scan_retained_object(value, state)
      binding <- list(
        name = name,
        active = read$active,
        type = typeof(value),
        closure = if (typeof(value) == "closure") list(
          environment = scan$root_environment,
          source = .slinker_analysis_binding(name, value)
        ) else NULL,
        environment = if (typeof(value) == "environment") scan$root_environment else NULL,
        closures = scan$closures,
        environments = scan$environments,
        issues = scan$issues
      )
    }
    record <- state$private_records[[id]]
    record$bindings[[length(record$bindings) + 1L]] <- binding
    state$private_records[[id]] <- record
  }

  state$private_status[[id]] <- 2L
  invisible(NULL)
}

.slinker_scan_retained_object <- function(value, state) {
  issues <- list()
  closures <- list()
  environments <- list()
  seen_envs <- list()

  add_issue <- function(path, kind, detail) {
    issues[[length(issues) + 1L]] <<- list(path = path, kind = kind, detail = detail)
  }
  seen_environment <- function(env) {
    if (length(seen_envs) && any(vapply(seen_envs, identical, logical(1L), y = env))) return(TRUE)
    seen_envs[[length(seen_envs) + 1L]] <<- env
    FALSE
  }
  environment_ref <- function(env) .slinker_environment_ref(state, env)
  add_closure <- function(path, value, env_ref) {
    closures[[length(closures) + 1L]] <<- list(
      path = path,
      environment = env_ref,
      source = paste0(
        ".slinker_embedded <- ",
        paste(deparse(value, width.cutoff = 500L, control = c("keepInteger", "keepNA", "niceNames")), collapse = "\n")
      )
    )
  }
  add_environment <- function(path, env_ref) {
    environments[[length(environments) + 1L]] <<- list(
      path = path,
      environment = env_ref
    )
  }

  walk <- function(x, path = "$", depth = 0L, embedded = FALSE) {
    if (depth > 128L) {
      add_issue(path, "object_depth", "object graph exceeds 128 levels")
      return(invisible(NULL))
    }
    type <- typeof(x)
    if (isS4(x)) add_issue(path, "s4", paste(class(x), collapse = "/"))
    classes <- class(x)
    if (length(classes) && any(grepl("^S7", classes))) add_issue(path, "s7", paste(classes, collapse = "/"))

    if (type == "closure") {
      env_ref <- environment_ref(environment(x))
      if (embedded) add_closure(path, x, env_ref)
      # A closure environment is lexical scope. Register its inventory so Rust
      # can resolve names through it, but never turn every binding into a child
      # of the closure merely because the environment is retained.
      if (startsWith(env_ref, "unsupported:")) {
        add_issue(paste0(path, ".environment"), "environment_identity", substring(env_ref, 13L))
      }
    } else if (type %in% c("list", "pairlist")) {
      values <- as.list(x)
      if (length(values)) {
        for (i in seq_along(values)) {
          if (!.slinker_is_missing_slot(values, i)) {
            walk(values[[i]], paste0(path, "[[", i, "]]"), depth + 1L, TRUE)
          }
        }
      }
    } else if (type %in% c("language", "expression")) {
      # Language and expression objects are data at inspection time. Do not
      # recursively promote symbols or function literals inside them to live
      # executable objects. Supported eval/parse semantics may activate them
      # later at the analysis layer.
    } else if (type == "environment") {
      ref <- environment_ref(x)
      if (embedded && !startsWith(ref, "unsupported:")) add_environment(path, ref)
      if (startsWith(ref, "unsupported:")) {
        add_issue(path, "environment_identity", substring(ref, 13L))
      } else if (startsWith(ref, "private:") && !seen_environment(x)) {
        # An environment retained as an ordinary object is itself observable,
        # unlike a closure's lexical environment. Conservatively retain its
        # contents as embedded object state.
        parent_ref <- environment_ref(parent.env(x))
        if (startsWith(parent_ref, "unsupported:")) {
          add_issue(paste0(path, ".parent"), "environment_parent", substring(parent_ref, 13L))
        }
        for (name in sort(ls(x, all.names = TRUE))) {
          read <- .slinker_read_binding_without_firing(x, name)
          if (inherits(read$value, "error")) {
            add_issue(paste0(path, "$", name), "force_error", conditionMessage(read$value))
          } else {
            walk(read$value, paste0(path, "$", name), depth + 1L, TRUE)
          }
        }
      }
    } else if (type == "externalptr") {
      add_issue(path, "external_pointer", "external pointer")
    } else if (type == "weakref") {
      add_issue(path, "weak_reference", "weak reference")
    } else if (!(type %in% c("NULL", "logical", "integer", "double", "complex", "character", "raw", "symbol", "builtin", "special"))) {
      add_issue(path, "unsupported_type", type)
    }

    if (!(type %in% c("language", "expression"))) {
      attrs <- attributes(x)
      if (length(attrs)) {
        for (name in names(attrs)) {
          walk(attrs[[name]], paste0(path, ".attr[", name, "]"), depth + 1L, TRUE)
        }
      }
    }
    invisible(NULL)
  }

  walk(value)
  root_environment <- if (typeof(value) == "closure") {
    environment_ref(environment(value))
  } else if (typeof(value) == "environment") {
    environment_ref(value)
  } else {
    NULL
  }
  list(
    issues = issues,
    closures = closures,
    environments = environments,
    root_environment = root_environment
  )
}

.slinker_emit_private_environments <- function(state, emit) {
  if (!length(state$private_records)) return(invisible(NULL))
  for (id in seq_along(state$private_records)) {
    record <- state$private_records[[id]]
    env_ref <- paste0("private:", id)
    emit("PRIVATE_ENV", env_ref, if (is.null(record$parent)) "base:empty" else record$parent)
    if (!length(record$bindings)) next
    for (binding in record$bindings) {
      emit(
        "PRIVATE_BINDING",
        env_ref,
        binding$name,
        binding$type,
        if (binding$active) "1" else "0"
      )
      if (!is.null(binding$environment)) {
        emit("PRIVATE_BINDING_ENV", env_ref, binding$name, binding$environment)
      }
      for (issue in binding$issues) {
        emit("PRIVATE_BINDING_ISSUE", env_ref, binding$name, issue$path, issue$kind, issue$detail)
      }
      for (nested_env in binding$environments) {
        emit(
          "PRIVATE_NESTED_ENV",
          env_ref,
          binding$name,
          nested_env$path,
          nested_env$environment
        )
      }
      for (closure in binding$closures) {
        emit(
          "PRIVATE_NESTED_CLOSURE",
          env_ref,
          binding$name,
          closure$path,
          closure$environment,
          closure$source
        )
      }
      if (!is.null(binding$closure)) {
        emit(
          "PRIVATE_CLOSURE",
          env_ref,
          binding$name,
          binding$closure$environment,
          binding$closure$source
        )
      }
    }
  }
  invisible(NULL)
}

slinker_inspect_image <- function(library, package, output, visible_libraries = character()) {
  context <- .slinker_package_image_context(library, package, visible_libraries)
  if (file.exists(output)) invisible(file.remove(output))
  connection <- file(output, open = "wt", encoding = "UTF-8")
  on.exit(close(connection), add = TRUE)
  emit <- function(kind, ...) .slinker_emit_connection(connection, kind, ...)

  .slinker_emit_package_index(context, emit, binding_record = NULL)
  scan_state <- .slinker_new_scan_state(context$image_env, context$package)

  for (name in context$binding_names) {
    origin <- if (name %in% context$sysdata_names) "sysdata" else "code"
    read <- .slinker_read_binding_without_firing(context$image_env, name)
    value <- read$value
    if (inherits(value, "error")) {
      emit("BINDING", name, origin, "unavailable", if (read$active) "1" else "0")
      emit("BINDING_ISSUE", name, "$", "force_error", conditionMessage(value))
      next
    }
    type <- typeof(value)
    emit("BINDING", name, origin, type, if (read$active) "1" else "0")
    scan <- .slinker_scan_retained_object(value, scan_state)
    if (identical(type, "environment")) emit("BINDING_ENV", name, scan$root_environment)
    for (issue in scan$issues) emit("BINDING_ISSUE", name, issue$path, issue$kind, issue$detail)
    for (nested_env in scan$environments) emit("NESTED_ENV", name, nested_env$path, nested_env$environment)
    for (closure in scan$closures) emit("NESTED_CLOSURE", name, closure$path, closure$environment, closure$source)
    if (identical(type, "closure")) {
      source <- .slinker_analysis_binding(name, value)
      emit("CLOSURE", name, scan$root_environment, source)
    }
  }

  .slinker_emit_private_environments(scan_state, emit)
  invisible(NULL)
}

.slinker_read_image_manifest <- function(path) {
  records <- strsplit(readLines(path, warn = FALSE), "\t", fixed = TRUE)
  libraries <- character()
  jobs <- list()

  for (record in records) {
    if (!length(record)) next
    kind <- record[[1L]]
    fields <- vapply(record[-1L], .slinker_unhex, character(1L), USE.NAMES = FALSE)
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

.slinker_inspect_image_job <- function(job) {
  slinker_inspect_image(job$library, job$package, job$output, job$visible_libraries)
  invisible(job$output)
}

slinker_inspect_image_batch <- function(manifest, jobs = 1L) {
  spec <- .slinker_read_image_manifest(manifest)
  if (!length(spec$jobs)) return(invisible(NULL))
  work <- lapply(spec$jobs, function(job) {
    job$visible_libraries <- spec$libraries
    job
  })
  invisible(.slinker_parallel_map(work, ".slinker_inspect_image_job", jobs))
}

.slinker_inspect_index_job <- function(job) {
  slinker_inspect_index(job$library, job$package, job$output, job$visible_libraries)
  invisible(job$output)
}

slinker_inspect_index_batch <- function(manifest, jobs = 1L) {
  spec <- .slinker_read_image_manifest(manifest)
  if (!length(spec$jobs)) return(invisible(NULL))
  work <- lapply(spec$jobs, function(job) {
    job$visible_libraries <- spec$libraries
    job
  })
  invisible(.slinker_parallel_map(work, ".slinker_inspect_index_job", jobs))
}
