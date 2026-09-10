.hrm_parallel_map <- function(x, fun_name, jobs) {
  if (!length(x)) return(list())

  jobs <- suppressWarnings(as.integer(jobs))
  if (length(jobs) != 1L || is.na(jobs) || jobs < 1L) stop("jobs must be a positive integer", call. = FALSE)
  jobs <- min(jobs, length(x))

  runtime <- environment(.hrm_parallel_map)
  fun <- get(fun_name, envir = runtime, inherits = FALSE)
  if (jobs == 1L) return(lapply(x, fun))

  if (.Platform$OS.type == "windows") {
    cl <- parallel::makePSOCKcluster(
      jobs,
      useXDR = FALSE,
      rscript_args = "--vanilla",
      methods = FALSE
    )
  } else {
    cl <- parallel::makeForkCluster(jobs)
  }
  on.exit(parallel::stopCluster(cl), add = TRUE)

  vendored <- exists(".hrm_source_files", envir = runtime, inherits = FALSE)
  if (.Platform$OS.type == "windows") {
    if (vendored) {
      init_worker <- function(source_files) {
        runtime <- new.env(parent = baseenv())
        for (file in source_files) sys.source(file, envir = runtime, keep.source = FALSE)
        assign(".hrm_runtime", runtime, envir = .GlobalEnv)
        invisible(NULL)
      }
      environment(init_worker) <- baseenv()
      parallel::clusterCall(cl, init_worker, get(".hrm_source_files", envir = runtime, inherits = FALSE))
    } else {
      load_worker_package <- function(package) {
        loadNamespace(package)
        invisible(NULL)
      }
      environment(load_worker_package) <- baseenv()
      parallel::clusterCall(cl, load_worker_package, "heRmetic")
    }
  }

  invoke_worker <- function(value, name, vendored) {
    runtime <- if (vendored) .GlobalEnv$.hrm_runtime else asNamespace("heRmetic")
    get(name, envir = runtime, inherits = FALSE)(value)
  }
  environment(invoke_worker) <- baseenv()
  parallel::parLapplyLB(cl, x, invoke_worker, fun_name, vendored)
}
