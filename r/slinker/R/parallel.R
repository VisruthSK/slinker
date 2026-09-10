.slinker_stop_parallel <- function() {
  runtime <- environment(.slinker_stop_parallel)
  if (exists(".slinker_cluster_handle", envir = runtime, inherits = FALSE)) {
    cluster <- get(".slinker_cluster_handle", envir = runtime, inherits = FALSE)
    try(parallel::stopCluster(cluster), silent = TRUE)
    rm(list = c(".slinker_cluster_handle", ".slinker_cluster_jobs"), envir = runtime)
  }
  invisible(NULL)
}

.slinker_cluster <- function(jobs) {
  runtime <- environment(.slinker_cluster)
  if (exists(".slinker_cluster_handle", envir = runtime, inherits = FALSE) &&
      identical(get(".slinker_cluster_jobs", envir = runtime, inherits = FALSE), jobs)) {
    return(get(".slinker_cluster_handle", envir = runtime, inherits = FALSE))
  }

  .slinker_stop_parallel()
  if (.Platform$OS.type == "windows") {
    cluster <- parallel::makePSOCKcluster(
      jobs,
      useXDR = FALSE,
      rscript_args = "--vanilla",
      methods = FALSE,
      homogeneous = TRUE,
      setup_strategy = "parallel"
    )
  } else {
    cluster <- parallel::makeForkCluster(jobs)
  }

  vendored <- exists(".slinker_source_files", envir = runtime, inherits = FALSE)
  if (.Platform$OS.type == "windows") {
    if (vendored) {
      init_worker <- function(source_files) {
        runtime <- new.env(parent = baseenv())
        for (file in source_files) sys.source(file, envir = runtime, keep.source = FALSE)
        assign(".slinker_runtime", runtime, envir = .GlobalEnv)
        invisible(NULL)
      }
      environment(init_worker) <- baseenv()
      parallel::clusterCall(
        cluster,
        init_worker,
        get(".slinker_source_files", envir = runtime, inherits = FALSE)
      )
    } else {
      load_worker_package <- function(package) {
        loadNamespace(package)
        invisible(NULL)
      }
      environment(load_worker_package) <- baseenv()
      parallel::clusterCall(cluster, load_worker_package, "slinker")
    }
  }

  assign(".slinker_cluster_handle", cluster, envir = runtime)
  assign(".slinker_cluster_jobs", jobs, envir = runtime)
  cluster
}

.slinker_parallel_map <- function(x, fun_name, jobs) {
  if (!length(x)) return(list())

  jobs <- suppressWarnings(as.integer(jobs))
  if (length(jobs) != 1L || is.na(jobs) || jobs < 1L) stop("jobs must be a positive integer", call. = FALSE)

  runtime <- environment(.slinker_parallel_map)
  fun <- get(fun_name, envir = runtime, inherits = FALSE)
  if (jobs == 1L || length(x) == 1L) return(lapply(x, fun))

  cluster <- .slinker_cluster(jobs)
  vendored <- exists(".slinker_source_files", envir = runtime, inherits = FALSE)
  invoke_worker <- function(value, name, vendored) {
    runtime <- if (vendored) .GlobalEnv$.slinker_runtime else asNamespace("slinker")
    get(name, envir = runtime, inherits = FALSE)(value)
  }
  environment(invoke_worker) <- baseenv()
  parallel::parLapplyLB(cluster, x, invoke_worker, fun_name, vendored)
}

.slinker_server_dispatch <- function(command, manifest, jobs) {
  # A forced installed binding should not normally print, but if one does, it
  # must not corrupt the stdout control protocol used by the persistent server.
  sink_depth <- sink.number(type = "output")
  sink(stderr(), type = "output")
  on.exit({
    while (sink.number(type = "output") > sink_depth) sink(type = "output")
  }, add = TRUE)
  if (identical(command, "INDEX")) {
    slinker_inspect_index_batch(manifest, jobs)
  } else if (identical(command, "IMAGE")) {
    slinker_inspect_image_batch(manifest, jobs)
  } else {
    stop(sprintf("unknown runtime-server request '%s'", command), call. = FALSE)
  }
  invisible(NULL)
}

slinker_runtime_server <- function(jobs) {
  input <- file("stdin", open = "r", encoding = "UTF-8")
  on.exit(close(input), add = TRUE)
  on.exit(.slinker_stop_parallel(), add = TRUE)

  repeat {
    line <- readLines(input, n = 1L, warn = FALSE)
    if (!length(line)) break
    fields <- strsplit(line, "\t", fixed = TRUE)[[1L]]
    command <- fields[[1L]]
    if (identical(command, "STOP")) break

    response <- tryCatch({
      if (length(fields) != 2L) stop("invalid runtime-server request", call. = FALSE)
      manifest <- .slinker_unhex(fields[[2L]])
      .slinker_server_dispatch(command, manifest, jobs)
      "OK"
    }, error = function(error) {
      .slinker_stop_parallel()
      paste0("ERROR\t", .slinker_hex(conditionMessage(error)))
    })

    writeLines(response, stdout(), sep = "\n", useBytes = TRUE)
    flush(stdout())
  }
  invisible(NULL)
}
