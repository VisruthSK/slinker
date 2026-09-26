.slinker_package_context <- function(root, package) {
  root <- normalizePath(root, winslash = "/", mustWork = TRUE)
  required <- file.path(
    root,
    c("DESCRIPTION", "Meta/nsInfo.rds", "Meta/package.rds")
  )
  if (!all(file.exists(required))) {
    stop(
      sprintf("installed package metadata missing under %s", root),
      call. = FALSE
    )
  }

  version <- unname(readRDS(file.path(
    root,
    "Meta",
    "package.rds"
  ))$DESCRIPTION[["Version"]])
  image_env <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
  info <- new.env(hash = TRUE, parent = baseenv())
  info$spec <- c(name = package, version = version)
  assign(".__NAMESPACE__.", info, envir = image_env)
  if (is.null(.Internal(getRegisteredNamespace(package)))) {
    .Internal(registerNamespace(package, image_env))
  }
  code_db <- file.path(root, "R", package)
  if (
    !file.exists(paste0(code_db, ".rdx")) ||
      !file.exists(paste0(code_db, ".rdb"))
  ) {
    stop(
      sprintf("installed R lazy-load database missing for %s", package),
      call. = FALSE
    )
  }
  base::lazyLoad(code_db, envir = image_env)
  assign(
    ".__NAMESPACE__.",
    get(".__NAMESPACE__.", envir = image_env),
    envir = image_env
  )

  before <- ls(image_env, all.names = TRUE)
  sysdata_db <- file.path(root, "R", "sysdata")
  if (
    file.exists(paste0(sysdata_db, ".rdx")) &&
      file.exists(paste0(sysdata_db, ".rdb"))
  ) {
    base::lazyLoad(sysdata_db, envir = image_env)
    sysdata_names <- setdiff(ls(image_env, all.names = TRUE), before)
  } else {
    sysdata_names <- character()
  }

  data_env <- new.env(hash = TRUE, parent = emptyenv())
  data_db <- file.path(root, "data", "Rdata")
  if (
    file.exists(paste0(data_db, ".rdx")) && file.exists(paste0(data_db, ".rdb"))
  ) {
    base::lazyLoad(data_db, envir = data_env)
  }

  list(
    package = package,
    root = root,
    version = version,
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

.slinker_native_library <- function(root, name) {
  library <- paste(
    c(
      "libs",
      if (nzchar(.Platform$r_arch)) .Platform$r_arch,
      paste0(name, .Platform$dynlib.ext)
    ),
    collapse = "/"
  )
  interfaces <- c(c = ".C", call = ".Call", fortran = ".Fortran", external = ".External")
  unregistered <- lapply(interfaces, function(interface) character())
  if (!file.exists(file.path(root, library))) {
    return(list(library = character(), routines = unregistered, force_symbols = NA))
  }
  loaded <- tryCatch(
    {
      dll <- dyn.load(file.path(root, library), local = TRUE)
      registered <- getDLLRegisteredRoutines(dll)
      list(
        routines = lapply(interfaces, function(interface) {
          sort(unique(as.character(names(registered[[interface]]))))
        }),
        force_symbols = if (is.logical(unclass(dll)$forceSymbols)) unclass(dll)$forceSymbols else NA
      )
    },
    error = function(error) list(routines = unregistered, force_symbols = NA)
  )
  c(list(library = library), loaded)
}

.slinker_deparse_binding <- function(name, value) {
  rhs <- paste(
    deparse(
      value,
      width.cutoff = 500L,
      control = c("keepInteger", "keepNA", "niceNames")
    ),
    collapse = "\n"
  )
  simple <- grepl("^[A-Za-z.][A-Za-z0-9._]*$", name) &&
    !grepl("^\\.[0-9]", name)
  lhs <- if (simple) {
    name
  } else {
    paste0("`", gsub("([`\\\\])", "\\\\\\1", name), "`")
  }
  paste0(lhs, " <- ", rhs)
}

.slinker_normalize_source <- function(source) {
  expressions <- parse(text = source, keep.source = FALSE)
  paste(
    vapply(
      expressions,
      function(expression) {
        paste(
          deparse(
            expression,
            width.cutoff = 500L,
            control = c("keepInteger", "keepNA", "niceNames")
          ),
          collapse = "\n"
        )
      },
      character(1L),
      USE.NAMES = FALSE
    ),
    collapse = "\n"
  )
}

.slinker_closure_home <- function(image, root, kinds, names) {
  if (!length(root)) {
    return(image)
  }
  value <- get(root, envir = image, inherits = FALSE)
  for (index in seq_along(kinds)) {
    value <- switch(
      kinds[[index]],
      environment = if (is.function(value)) environment(value) else value,
      parent = parent.env(value),
      binding = .slinker_binding_value(value, names[[index]])
    )
  }
  if (!is.environment(value)) {
    stop("payload closure home is not an environment", call. = FALSE)
  }
  value
}

.slinker_binding_value <- function(environment, name) {
  if (bindingIsActive(name, environment)) {
    stop(sprintf("payload path crosses active binding %s", name), call. = FALSE)
  }
  get(name, envir = environment, inherits = FALSE)
}

.slinker_closure_at <- function(home, binding) {
  closure <- .slinker_binding_value(home, binding)
  if (typeof(closure) != "closure") {
    stop(sprintf("payload binding %s is not a closure", binding), call. = FALSE)
  }
  closure
}

.slinker_patch_closure <- function(home, binding, old, source) {
  expression <- parse(text = source, keep.source = FALSE)
  if (
    length(expression) != 1L ||
      !is.call(expression[[1L]]) ||
      !identical(expression[[1L]][[1L]], as.name("function"))
  ) {
    stop("rewritten payload closure source is not a function", call. = FALSE)
  }
  definition <- expression[[1L]]
  new <- as.function(
    c(as.list(definition[[2L]]), list(definition[[3L]])),
    envir = environment(old)
  )
  attributes(new) <- attributes(old)
  attr(new, "srcref") <- NULL
  locked <- bindingIsLocked(binding, home)
  if (locked) {
    unlockBinding(binding, home)
  }
  assign(binding, new, envir = home)
  if (locked) {
    lockBinding(binding, home)
  }
  invisible(NULL)
}

.slinker_payloads <- function(images, packages, registered, sources, names) {
  for (index in seq_along(images)) {
    if (!identical(.Internal(getRegisteredNamespace(packages[[index]])), images[[index]])) {
      stop(
        sprintf("namespace %s is not registered as its installed image", packages[[index]]),
        call. = FALSE
      )
    }
  }
  infos <- lapply(images, function(image) get(".__NAMESPACE__.", envir = image, inherits = FALSE))
  rename <- function(info, name) info$spec[["name"]] <- name
  on.exit(Map(rename, infos, packages), add = TRUE)
  Map(rename, infos, registered)
  Map(.slinker_bundle, sources, names)
}

.slinker_bundle <- function(image_env, names) {
  active <- vapply(names, bindingIsActive, logical(1L), env = image_env)
  if (any(active)) {
    stop(
      sprintf(
        "active binding %s cannot be serialized without execution",
        names[active][[1L]]
      ),
      call. = FALSE
    )
  }
  bundle <- .slinker_serialize(mget(names, envir = image_env, inherits = FALSE))
  bundle$namespaces <- .slinker_serialized_namespaces(bundle$bytes)
  bundle
}

.slinker_serialize <- function(value) {
  references <- vector("list", 64L)
  count <- 0L
  record <- function(object) {
    count <<- count + 1L
    if (count > length(references)) {
      length(references) <<- 2L * length(references)
    }
    references[[count]] <<- object
    NULL
  }
  bytes <- serialize(value, NULL, version = 3L, refhook = record)
  list(bytes = bytes, references = references[seq_len(count)])
}

.slinker_serialized_namespaces <- function(bytes) {
  found <- character()
  original <- get("..getNamespace", envir = baseenv(), inherits = FALSE)
  # unserialize() resolves each namespace reference through base's ..getNamespace; a recorder
  # compiled in advance avoids JIT compilation inside unserialize(), which fails.
  recorder <- compiler::cmpfun(function(name, where) {
    found <<- c(found, name[[1L]])
    emptyenv()
  })
  unlockBinding("..getNamespace", baseenv())
  on.exit(
    {
      assign("..getNamespace", original, envir = baseenv())
      lockBinding("..getNamespace", baseenv())
    },
    add = TRUE
  )
  assign("..getNamespace", recorder, envir = baseenv())
  unserialize(bytes)
  unique(found)
}
