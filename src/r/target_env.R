args <- commandArgs(trailingOnly = TRUE)
if (length(args) < 1L) stop("usage: target_env.R OUTPUT [LIBRARY ...]", call. = FALSE)
output <- args[[1L]]
requested <- args[-1L]

hex <- function(x) {
  x <- enc2utf8(as.character(x))
  paste(sprintf("%02x", as.integer(charToRaw(x))), collapse = "")
}
emit <- function(kind, ...) {
  fields <- vapply(list(...), hex, character(1L), USE.NAMES = FALSE)
  cat(paste(c(kind, fields), collapse = "\t"), "\n", file = output, append = TRUE, sep = "")
}

if (file.exists(output)) invisible(file.remove(output))
libs <- if (length(requested)) {
  # Explicit --lib paths define the non-base target universe. Base R's
  # installation library is always present because R itself requires it.
  c(requested, .Library)
} else {
  .libPaths()
}
libs <- vapply(libs, normalizePath, character(1L), winslash = "/", mustWork = TRUE)
lib_keys <- if (.Platform$OS.type == "windows") tolower(libs) else libs
libs <- libs[!duplicated(lib_keys)]

emit("HEADER", paste0(R.version$major, ".", R.version$minor), R.version$os, R.version$arch)
for (i in seq_along(libs)) emit("LIB", as.character(i - 1L), libs[[i]])

seen <- character()
for (lib in libs) {
  matrix <- utils::installed.packages(lib.loc = lib, fields = "Version", noCache = TRUE)
  if (!nrow(matrix)) next
  for (i in seq_len(nrow(matrix))) {
    name <- rownames(matrix)[[i]]
    if (name %in% seen) next
    seen <- c(seen, name)
    emit("PACKAGE", name, matrix[i, "Version"], lib)
  }
}
