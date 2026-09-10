.hrm_missing_arg_box <- as.list(alist(.hrm_missing = ))
names(.hrm_missing_arg_box) <- NULL

.hrm_is_missing_slot <- function(values, i) {
  slot <- values[i]
  names(slot) <- NULL
  identical(slot, .hrm_missing_arg_box)
}

.hrm_transform_slots <- function(x, transform) {
  values <- as.list(x)
  out <- vector("list", length(values))
  if (length(values)) {
    for (i in seq_along(values)) {
      if (.hrm_is_missing_slot(values, i)) {
        out[i] <- values[i]
      } else {
        out[i] <- list(transform(values[[i]]))
      }
    }
  }
  names(out) <- names(values)
  out
}

.hrm_transform_attributes <- function(out, source, transform, omit = NULL) {
  attrs <- attributes(source)
  if (!length(attrs)) {
    attributes(out) <- NULL
    return(out)
  }
  if (length(omit)) attrs[omit] <- NULL
  if (length(attrs)) {
    for (name in names(attrs)) attrs[name] <- list(transform(attrs[[name]]))
    attributes(out) <- attrs
  } else {
    attributes(out) <- NULL
  }
  out
}

.hrm_scrub_recipe <- function(x, environment_ref) {
  transform <- function(value) .hrm_scrub_recipe(value, environment_ref)
  type <- typeof(x)

  if (type == "closure") {
    out <- x
    ref <- environment_ref(environment(out))
    environment(out) <- emptyenv()
    out <- .hrm_transform_attributes(out, x, transform)
    attr(out, ".__hrm_recipe_env__.") <- ref
    return(out)
  }

  if (type == "list") {
    out <- .hrm_transform_slots(x, transform)
  } else if (type == "expression") {
    out <- as.expression(.hrm_transform_slots(x, transform))
  } else if (type == "pairlist") {
    out <- as.pairlist(.hrm_transform_slots(x, transform))
  } else if (type == "language") {
    out <- as.call(.hrm_transform_slots(x, transform))
  } else {
    out <- x
  }

  .hrm_transform_attributes(out, x, transform)
}

.hrm_rehydrate_recipe <- function(x, resolve_namespace) {
  transform <- function(value) .hrm_rehydrate_recipe(value, resolve_namespace)
  type <- typeof(x)
  marker <- ".__hrm_recipe_env__."

  if (type == "closure") {
    ref <- attr(x, marker, exact = TRUE)
    if (is.null(ref)) stop("closure recipe has no environment reference", call. = FALSE)
    out <- .hrm_transform_attributes(x, x, transform, marker)
    if (startsWith(ref, "namespace:")) {
      environment(out) <- resolve_namespace(substring(ref, 11L))
    } else if (identical(ref, "base:base")) {
      environment(out) <- baseenv()
    } else {
      stop(sprintf("unknown closure environment recipe '%s'", ref), call. = FALSE)
    }
    return(out)
  }

  if (type == "list") {
    out <- .hrm_transform_slots(x, transform)
  } else if (type == "expression") {
    out <- as.expression(.hrm_transform_slots(x, transform))
  } else if (type == "pairlist") {
    out <- as.pairlist(.hrm_transform_slots(x, transform))
  } else if (type == "language") {
    out <- as.call(.hrm_transform_slots(x, transform))
  } else {
    out <- x
  }

  .hrm_transform_attributes(out, x, transform)
}
