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
  if (!file.exists(file.path(root, library))) {
    return(list(library = character(), routines = character()))
  }
  routines <- tryCatch(
    unlist(lapply(
      getDLLRegisteredRoutines(dyn.load(
        file.path(root, library),
        local = TRUE
      )),
      names
    )),
    error = function(error) character()
  )
  list(library = library, routines = sort(unique(as.character(routines))))
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
  serialize(
    mget(names, envir = image_env, inherits = FALSE),
    NULL,
    version = 3L
  )
}
