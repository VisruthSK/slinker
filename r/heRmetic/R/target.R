hrm_capture_target <- function(output, requested = character()) {
  if (file.exists(output)) invisible(file.remove(output))

  libs <- if (length(requested)) c(requested, .Library) else .libPaths()
  libs <- .hrm_dedupe_libraries(libs)

  .hrm_emit(output, "HEADER", paste0(R.version$major, ".", R.version$minor), R.version$os, R.version$arch)
  for (i in seq_along(libs)) .hrm_emit(output, "LIB", as.character(i - 1L), libs[[i]])

  seen <- character()
  for (lib in libs) {
    matrix <- utils::installed.packages(lib.loc = lib, fields = "Version", noCache = TRUE)
    if (!nrow(matrix)) next
    for (i in seq_len(nrow(matrix))) {
      name <- rownames(matrix)[[i]]
      if (name %in% seen) next
      seen <- c(seen, name)
      .hrm_emit(output, "PACKAGE", name, matrix[i, "Version"], lib)
    }
  }
  invisible(NULL)
}
