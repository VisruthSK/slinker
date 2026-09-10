slinker_capture_target <- function(output, requested = character()) {
  libs <- if (length(requested)) c(requested, .Library) else .libPaths()
  libs <- .slinker_dedupe_libraries(libs)

  if (file.exists(output)) invisible(file.remove(output))
  connection <- file(output, open = "wt", encoding = "UTF-8")
  on.exit(close(connection), add = TRUE)
  emit <- function(kind, ...) .slinker_emit_connection(connection, kind, ...)

  emit("HEADER", paste0(R.version$major, ".", R.version$minor), R.version$os, R.version$arch)
  for (i in seq_along(libs)) emit("LIB", as.character(i - 1L), libs[[i]])
  for (name in sort(ls(.BaseNamespaceEnv, all.names = TRUE))) emit("BASE_BINDING", name)

  # Package discovery is demand-driven in Rust. Do not scan the installed
  # library universe here: large user libraries made target capture dominate
  # analysis before a single semantic need had been processed.
  invisible(NULL)
}
