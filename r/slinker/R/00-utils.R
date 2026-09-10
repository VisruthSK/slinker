.slinker_normalize_library <- function(path) {
  normalizePath(path, winslash = "/", mustWork = TRUE)
}

.slinker_path_key <- function(path) {
  normalized <- normalizePath(path, winslash = "/", mustWork = FALSE)
  if (.Platform$OS.type == "windows") tolower(normalized) else normalized
}

.slinker_dedupe_libraries <- function(paths) {
  normalized <- vapply(paths, .slinker_normalize_library, character(1L))
  normalized[!duplicated(vapply(normalized, .slinker_path_key, character(1L)))]
}

.slinker_hex <- function(x) {
  x <- enc2utf8(as.character(x))
  paste(as.character(charToRaw(x)), collapse = "")
}

.slinker_unhex <- function(x) {
  if (!nzchar(x)) return("")
  if (nchar(x) %% 2L) stop("invalid odd-length hex field", call. = FALSE)
  bytes <- substring(x, seq.int(1L, nchar(x), 2L), seq.int(2L, nchar(x), 2L))
  rawToChar(as.raw(strtoi(bytes, 16L)))
}

.slinker_quote_binding <- function(name) {
  simple <- grepl("^[A-Za-z.][A-Za-z0-9._]*$", name) && !grepl("^\\.[0-9]", name)
  if (simple) name else paste0("`", gsub("`", "\\\\`", name, fixed = TRUE), "`")
}

.slinker_emit_connection <- function(connection, kind, ...) {
  fields <- vapply(list(...), .slinker_hex, character(1L), USE.NAMES = FALSE)
  cat(paste(c(kind, fields), collapse = "\t"), "\n", file = connection, sep = "")
}

.slinker_analysis_binding <- function(name, value) {
  lhs <- .slinker_quote_binding(name)
  rhs <- if (typeof(value) == "closure") {
    paste(deparse(value, width.cutoff = 500L, control = c("keepInteger", "keepNA", "niceNames")), collapse = "\n")
  } else {
    "NULL"
  }
  paste0(lhs, " <- ", rhs)
}

.slinker_missing_arg_box <- as.list(alist(.slinker_missing = ))
names(.slinker_missing_arg_box) <- NULL

.slinker_is_missing_slot <- function(values, i) {
  slot <- values[i]
  names(slot) <- NULL
  identical(slot, .slinker_missing_arg_box)
}
