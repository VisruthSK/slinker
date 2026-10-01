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
  if (!file.exists(file.path(root, library))) {
    return(list(library = character()))
  }
  tryCatch(
    {
      dll <- dyn.load(file.path(root, library), local = TRUE)
      registered <- getDLLRegisteredRoutines(dll)
      list(
        library = library,
        routines = lapply(interfaces, function(interface) {
          sort(unique(as.character(names(registered[[interface]]))))
        }),
        force_symbols = isTRUE(unclass(dll)$forceSymbols)
      )
    },
    error = function(error) list(library = library, error = conditionMessage(error))
  )
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

.slinker_verify_relocation <- function(original, rewritten, starts, ends,
                                       replacements, appended_names,
                                       appended_values) {
  fail <- function(...) stop(paste0(...), call. = FALSE)
  parse_tree <- function(text) as.list(parse(text = text, keep.source = FALSE))
  parse_one <- function(text, role) {
    tree <- parse_tree(text)
    if (length(tree) != 1L) {
      fail(role, " is not exactly one R expression: ", text)
    }
    tree
  }
  original <- enc2utf8(original)
  bytes <- charToRaw(original)
  starts <- as.integer(starts)
  ends <- as.integer(ends)
  count <- length(starts)
  if (length(ends) != count || length(replacements) != count ||
    length(appended_names) != count || length(appended_values) != count) {
    fail("relocation site fields have different lengths")
  }
  if (anyNA(starts) || anyNA(ends) || any(starts < 0L) || any(ends <= starts) ||
    any(ends > length(bytes))) {
    fail("relocation site ranges lie outside the original code")
  }
  ordered <- order(starts)
  if (count > 1L && any(starts[ordered][-1L] < ends[ordered][-count])) {
    fail("relocation sites overlap")
  }
  slice <- function(from, to) {
    if (to < from) {
      return("")
    }
    text <- rawToChar(bytes[from:to])
    Encoding(text) <- "UTF-8"
    text
  }
  prefix <- ".slinker_site_"
  while (grepl(prefix, original, fixed = TRUE)) {
    prefix <- paste0(prefix, "_")
  }
  placeholders <- paste0(prefix, seq_len(count))
  pieces <- character()
  cursor <- 1L
  for (site in ordered) {
    pieces <- c(pieces, slice(cursor, starts[[site]]), placeholders[[site]])
    cursor <- ends[[site]] + 1L
  }
  template <- paste(c(pieces, slice(cursor, length(bytes))), collapse = "")
  original_sites <- lapply(seq_len(count), function(site) {
    parse_one(slice(starts[[site]] + 1L, ends[[site]]), "relocation site")
  })
  replacement_sites <- lapply(seq_len(count), function(site) {
    parse_one(replacements[[site]], "relocation replacement")
  })
  appended <- lapply(seq_len(count), function(site) {
    if (!nzchar(appended_names[[site]])) {
      return(NULL)
    }
    argument <- parse_one(appended_values[[site]], "appended argument")
    names(argument) <- appended_names[[site]]
    argument
  })

  substitute_sites <- function(tree, values, appended) {
    seen <- integer(count)
    rebuild <- function(items, kind) {
      labels <- names(items)
      out <- list()
      for (i in seq_along(items)) {
        label <- if (is.null(labels)) "" else labels[[i]]
        site <- if (is.symbol(items[[i]])) {
          match(as.character(items[[i]]), placeholders, nomatch = 0L)
        } else {
          0L
        }
        if (site > 0L) {
          seen[[site]] <<- seen[[site]] + 1L
          out <- c(out, `names<-`(values[[site]], label))
          if (!is.null(appended[[site]])) {
            if (kind != "call" || i == 1L) {
              fail("relocation site ", site, " is not an argument of a call")
            }
            out <- c(out, appended[[site]])
          }
        } else if (is.call(items[[i]])) {
          out <- c(out, `names<-`(list(rebuild(as.list(items[[i]]), "call")), label))
        } else if (is.pairlist(items[[i]]) && length(items[[i]])) {
          out <- c(out, `names<-`(list(rebuild(as.list(items[[i]]), "pairlist")), label))
        } else {
          out <- c(out, items[i])
        }
      }
      if (all(names(out) == "")) {
        names(out) <- NULL
      }
      switch(kind,
        call = as.call(out),
        pairlist = as.pairlist(out),
        expression = out
      )
    }
    rebuilt <- rebuild(tree, "expression")
    if (any(seen != 1L)) {
      fail(
        "relocation site ", which(seen != 1L)[[1L]],
        " is not one whole expression of the original code"
      )
    }
    rebuilt
  }

  template_tree <- parse_tree(template)
  restored <- substitute_sites(template_tree, original_sites, vector("list", count))
  if (!identical(restored, parse_tree(original))) {
    fail("relocation sites are not whole expressions of the original code")
  }
  expected <- substitute_sites(template_tree, replacement_sites, appended)
  if (!identical(parse_tree(rewritten), expected)) {
    fail("rewritten code differs from the original code with its planned replacements")
  }
  TRUE
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

.slinker_s3_groups <- list(
  Math = c(
    "abs", "sign", "sqrt", "floor", "ceiling", "trunc", "round", "signif",
    "exp", "log", "expm1", "log1p", "cos", "sin", "tan", "cospi", "sinpi",
    "tanpi", "acos", "asin", "atan", "cosh", "sinh", "tanh", "acosh", "asinh",
    "atanh", "lgamma", "gamma", "digamma", "trigamma", "cumsum", "cumprod",
    "cummax", "cummin", "log2", "log10"
  ),
  Ops = c(
    "+", "-", "*", "/", "^", "%%", "%/%", "&", "|", "!", "==", "!=", "<", "<=",
    ">=", ">"
  ),
  matrixOps = "%*%",
  Summary = c("all", "any", "sum", "prod", "min", "max", "range"),
  Complex = c("Arg", "Conj", "Im", "Mod", "Re")
)

.slinker_s3_aliases <- list(as.numeric = "as.double", seq.int = "seq")

.slinker_use_method_generics <- function(expression) {
  generics <- character()
  visit <- function(call) {
    head <- call[[1L]]
    is_use_method <- identical(head, quote(UseMethod)) ||
      identical(head, quote(base::UseMethod))
    if (
      is_use_method &&
        length(call) >= 2L &&
        is.character(call[[2L]]) &&
        length(call[[2L]]) == 1L
    ) {
      generics <<- c(generics, call[[2L]])
    }
    for (index in seq_along(call)) {
      if (is.call(call[[index]])) visit(call[[index]])
    }
  }
  if (is.call(expression)) visit(expression)
  unique(generics)
}

.slinker_dispatch_generics <- function(environment, name) {
  value <- get(name, envir = environment, inherits = FALSE)
  if (!is.function(value)) {
    return(character())
  }
  if (is.primitive(value)) {
    groups <- names(Filter(function(members) name %in% members, .slinker_s3_groups))
    return(unique(c(name, groups, .slinker_s3_aliases[[name]])))
  }
  unique(c(
    .slinker_use_method_generics(body(value)),
    if (name %in% .internalGenerics) name
  ))
}
