ns <- asNamespace("heRmetic")
parallel_map <- get(".hrm_parallel_map", envir = ns)

result <- parallel_map(c("plain", "not a name"), ".hrm_quote_binding", 1L)
stopifnot(identical(result, list("plain", "`not a name`")))

result <- parallel_map(c("plain", "not a name"), ".hrm_quote_binding", 2L)
stopifnot(identical(result, list("plain", "`not a name`")))
