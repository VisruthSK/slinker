ns <- asNamespace("heRmetic")
scrub <- get(".hrm_scrub_recipe", envir = ns)
rehydrate <- get(".hrm_rehydrate_recipe", envir = ns)

roundtrip <- function(x) {
  recipe <- scrub(x, function(env) {
    if (identical(env, baseenv())) "base:base" else "namespace:base"
  })
  rehydrate(recipe, function(name) {
    if (!identical(name, "base")) stop("unexpected namespace", call. = FALSE)
    .BaseNamespaceEnv
  })
}

x <- list(a = 1L, b = NULL, c = 3L)
stopifnot(identical(roundtrip(x), x))

f <- function(x, out, z = NULL) x
stopifnot(identical(formals(roundtrip(f)), formals(f)))

call <- quote(f(1, , 3))
stopifnot(identical(roundtrip(call), call))

recipes <- list(bindings = list())
recipes$bindings["nil"] <- list(NULL)
stopifnot("nil" %in% names(recipes$bindings), is.null(recipes$bindings[["nil"]]))
