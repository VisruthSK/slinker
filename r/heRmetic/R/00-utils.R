.hrm_normalize_library <- function(path) {
  normalizePath(path, winslash = "/", mustWork = TRUE)
}

.hrm_path_key <- function(path) {
  normalized <- normalizePath(path, winslash = "/", mustWork = FALSE)
  if (.Platform$OS.type == "windows") tolower(normalized) else normalized
}

.hrm_dedupe_libraries <- function(paths) {
  normalized <- vapply(paths, .hrm_normalize_library, character(1L))
  normalized[!duplicated(vapply(normalized, .hrm_path_key, character(1L)))]
}

.hrm_hex <- function(x) {
  x <- enc2utf8(as.character(x))
  paste(sprintf("%02x", as.integer(charToRaw(x))), collapse = "")
}

.hrm_unhex <- function(x) {
  if (!nzchar(x)) return("")
  if (nchar(x) %% 2L) stop("invalid odd-length hex field", call. = FALSE)
  bytes <- substring(x, seq.int(1L, nchar(x), 2L), seq.int(2L, nchar(x), 2L))
  rawToChar(as.raw(strtoi(bytes, 16L)))
}

.hrm_emit <- function(output, kind, ...) {
  fields <- vapply(list(...), .hrm_hex, character(1L), USE.NAMES = FALSE)
  cat(paste(c(kind, fields), collapse = "\t"), "\n", file = output, append = TRUE, sep = "")
}

.hrm_read_inspection_manifest <- function(path) {
  records <- strsplit(readLines(path, warn = FALSE), "\t", fixed = TRUE)
  libraries <- character()
  jobs <- list()

  for (record in records) {
    if (!length(record)) next
    kind <- record[[1L]]
    fields <- vapply(record[-1L], .hrm_unhex, character(1L), USE.NAMES = FALSE)
    if (identical(kind, "LIB")) {
      if (length(fields) != 1L) stop("invalid LIB record", call. = FALSE)
      libraries <- c(libraries, fields[[1L]])
    } else if (identical(kind, "JOB")) {
      if (length(fields) != 5L) stop("invalid JOB record", call. = FALSE)
      jobs[[length(jobs) + 1L]] <- list(
        library = fields[[1L]],
        package = fields[[2L]],
        output = fields[[3L]],
        recipes_output = fields[[4L]],
        analysis_dir = fields[[5L]]
      )
    } else {
      stop(sprintf("unknown inspection manifest record '%s'", kind), call. = FALSE)
    }
  }

  list(libraries = libraries, jobs = jobs)
}

.hrm_quote_binding <- function(name) {
  simple <- grepl("^[A-Za-z.][A-Za-z0-9._]*$", name) && !grepl("^\\.[0-9]", name)
  if (simple) name else paste0("`", gsub("`", "\\\\`", name, fixed = TRUE), "`")
}

.hrm_emit_connection <- function(connection, kind, ...) {
  fields <- vapply(list(...), .hrm_hex, character(1L), USE.NAMES = FALSE)
  cat(paste(c(kind, fields), collapse = "\t"), "\n", file = connection, sep = "")
}
