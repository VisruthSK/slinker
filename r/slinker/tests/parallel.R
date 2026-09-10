ns <- asNamespace("slinker")
parallel_map <- get(".slinker_parallel_map", envir = ns)
parallel_stop <- get(".slinker_stop_parallel", envir = ns)
server_dispatch <- get(".slinker_server_dispatch", envir = ns)

result <- parallel_map(c("plain", "not a name"), ".slinker_quote_binding", 1L)
stopifnot(identical(result, list("plain", "`not a name`")))

result <- parallel_map(c("plain", "not a name"), ".slinker_quote_binding", 2L)
stopifnot(identical(result, list("plain", "`not a name`")))

# A second parallel batch exercises reuse of the already-created worker pool.
result <- parallel_map(c("another name", "ok"), ".slinker_quote_binding", 2L)
stopifnot(identical(result, list("`another name`", "ok")))

# Server dispatch must restore stdout sinking even when dispatch fails.
sink_depth <- sink.number(type = "output")
error <- try(server_dispatch("INVALID", "unused", 1L), silent = TRUE)
stopifnot(inherits(error, "try-error"))
stopifnot(identical(sink.number(type = "output"), sink_depth))

parallel_stop()
