\name{hrm_inspect_batch}
\alias{hrm_inspect_batch}
\title{Inspect installed package images}
\usage{hrm_inspect_batch(manifest, jobs = 1L)}
\arguments{
  \item{manifest}{Path to an inspection job manifest.}
  \item{jobs}{Maximum number of local worker processes.}
}
\description{Inspects installed lazy-load images in parallel and writes independent package analysis shards.}
\value{Invisibly returns \code{NULL}.}
\keyword{internal}
