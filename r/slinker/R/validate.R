slinker_validate_syntax <- function(source_path, result_path) {
  source <- paste(readLines(source_path, warn = FALSE, encoding = "UTF-8"), collapse = "\n")
  result <- tryCatch({
    parse(text = source, keep.source = TRUE)
    "OK"
  }, error = function(error) paste0("ERROR\t", .slinker_hex(conditionMessage(error))))
  writeLines(result, result_path, useBytes = TRUE)
  invisible(NULL)
}
