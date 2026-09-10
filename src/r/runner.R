args <- commandArgs(trailingOnly = TRUE)
if (length(args) < 2L) stop("usage: runner.R PACKAGE_ROOT COMMAND [ARG ...]", call. = FALSE)

package_root <- normalizePath(args[[1L]], winslash = "/", mustWork = TRUE)
command <- args[[2L]]
command_args <- args[-c(1L, 2L)]
source_files <- sort(list.files(file.path(package_root, "R"), pattern = "\\.[Rr]$", full.names = TRUE))
if (!length(source_files)) stop("vendored heRmetic R package has no R sources", call. = FALSE)

runtime <- new.env(parent = baseenv())
for (file in source_files) sys.source(file, envir = runtime, keep.source = FALSE)
assign(".hrm_source_files", source_files, envir = runtime)
assign(".hrm_runtime", runtime, envir = .GlobalEnv)

if (identical(command, "capture-target")) {
  if (!length(command_args)) stop("capture-target requires OUTPUT [LIBRARY ...]", call. = FALSE)
  runtime$hrm_capture_target(command_args[[1L]], command_args[-1L])
} else if (identical(command, "inspect-batch")) {
  if (length(command_args) != 2L) stop("inspect-batch requires MANIFEST JOBS", call. = FALSE)
  runtime$hrm_inspect_batch(command_args[[1L]], as.integer(command_args[[2L]]))
} else if (identical(command, "inspect-index")) {
  if (length(command_args) < 3L) stop("inspect-index requires LIBRARY PACKAGE OUTPUT [VISIBLE_LIBRARY ...]", call. = FALSE)
  runtime$hrm_inspect_index(command_args[[1L]], command_args[[2L]], command_args[[3L]], command_args[-c(1L, 2L, 3L)])
} else if (identical(command, "inspect-image")) {
  if (length(command_args) < 3L) stop("inspect-image requires LIBRARY PACKAGE OUTPUT [VISIBLE_LIBRARY ...]", call. = FALSE)
  runtime$hrm_inspect_image(command_args[[1L]], command_args[[2L]], command_args[[3L]], command_args[-c(1L, 2L, 3L)])
} else if (identical(command, "inspect-index-batch")) {
  if (length(command_args) != 2L) stop("inspect-index-batch requires MANIFEST JOBS", call. = FALSE)
  runtime$hrm_inspect_index_batch(command_args[[1L]], as.integer(command_args[[2L]]))
} else if (identical(command, "inspect-image-batch")) {
  if (length(command_args) != 2L) stop("inspect-image-batch requires MANIFEST JOBS", call. = FALSE)
  runtime$hrm_inspect_image_batch(command_args[[1L]], as.integer(command_args[[2L]]))
} else if (identical(command, "validate-syntax")) {
  if (length(command_args) != 2L) stop("validate-syntax requires SOURCE RESULT", call. = FALSE)
  runtime$hrm_validate_syntax(command_args[[1L]], command_args[[2L]])
} else if (identical(command, "materialize")) {
  if (length(command_args) != 1L) stop("materialize requires SPEC", call. = FALSE)
  runtime$hrm_materialize(command_args[[1L]])
} else {
  stop(sprintf("unknown heRmetic R runtime command '%s'", command), call. = FALSE)
}
