\name{hrm_capture_target}
\alias{hrm_capture_target}
\title{Capture an R target environment}
\usage{hrm_capture_target(output, requested = character())}
\arguments{
  \item{output}{Path to the target-environment protocol output.}
  \item{requested}{Optional ordered library paths.}
}
\description{Captures the selected R library universe and installed package identities for the heRmetic linker.}
\value{Invisibly returns \code{NULL}.}
\keyword{internal}
