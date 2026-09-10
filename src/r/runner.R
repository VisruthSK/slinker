args <- commandArgs(trailingOnly = TRUE)
if (length(args) < 2L) stop("usage: runner.R PACKAGE_ROOT COMMAND [ARG ...]", call. = FALSE)

package_root <- normalizePath(args[[1L]], winslash = "/", mustWork = TRUE)
command <- args[[2L]]
command_args <- args[-c(1L, 2L)]
r_dir <- file.path(package_root, "R")

files_for <- switch(
  command,
  "capture-target" = c("00-utils.R", "target.R"),
  "inspect-index" = c("00-utils.R", "image.R"),
  "inspect-image" = c("00-utils.R", "image.R"),
  "inspect-index-batch" = c("00-utils.R", "image.R", "parallel.R"),
  "inspect-image-batch" = c("00-utils.R", "image.R", "parallel.R"),
  "serve" = c("00-utils.R", "image.R", "parallel.R"),
  "validate-syntax" = c("00-utils.R", "validate.R"),
  stop(sprintf("unknown slinker R runtime command '%s'", command), call. = FALSE)
)
source_files <- file.path(r_dir, files_for)
missing <- !file.exists(source_files)
if (any(missing)) stop(sprintf("vendored slinker R source missing: %s", paste(source_files[missing], collapse = ", ")), call. = FALSE)

runtime <- new.env(parent = baseenv())
for (file in source_files) sys.source(file, envir = runtime, keep.source = FALSE)
assign(".slinker_source_files", source_files, envir = runtime)
assign(".slinker_runtime", runtime, envir = .GlobalEnv)

if (identical(command, "capture-target")) {
  if (!length(command_args)) stop("capture-target requires OUTPUT [LIBRARY ...]", call. = FALSE)
  runtime$slinker_capture_target(command_args[[1L]], command_args[-1L])
} else if (identical(command, "inspect-index")) {
  if (length(command_args) < 3L) stop("inspect-index requires LIBRARY PACKAGE OUTPUT [VISIBLE_LIBRARY ...]", call. = FALSE)
  runtime$slinker_inspect_index(command_args[[1L]], command_args[[2L]], command_args[[3L]], command_args[-c(1L, 2L, 3L)])
} else if (identical(command, "inspect-image")) {
  if (length(command_args) < 3L) stop("inspect-image requires LIBRARY PACKAGE OUTPUT [VISIBLE_LIBRARY ...]", call. = FALSE)
  runtime$slinker_inspect_image(command_args[[1L]], command_args[[2L]], command_args[[3L]], command_args[-c(1L, 2L, 3L)])
} else if (identical(command, "inspect-index-batch")) {
  if (length(command_args) != 2L) stop("inspect-index-batch requires MANIFEST JOBS", call. = FALSE)
  runtime$slinker_inspect_index_batch(command_args[[1L]], as.integer(command_args[[2L]]))
} else if (identical(command, "inspect-image-batch")) {
  if (length(command_args) != 2L) stop("inspect-image-batch requires MANIFEST JOBS", call. = FALSE)
  runtime$slinker_inspect_image_batch(command_args[[1L]], as.integer(command_args[[2L]]))
} else if (identical(command, "serve")) {
  if (length(command_args) != 1L) stop("serve requires JOBS", call. = FALSE)
  runtime$slinker_runtime_server(as.integer(command_args[[1L]]))
} else if (identical(command, "validate-syntax")) {
  if (length(command_args) != 2L) stop("validate-syntax requires SOURCE RESULT", call. = FALSE)
  runtime$slinker_validate_syntax(command_args[[1L]], command_args[[2L]])
}
