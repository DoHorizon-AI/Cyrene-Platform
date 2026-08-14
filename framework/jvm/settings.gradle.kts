rootProject.name = "cyrene-control-plane"

include("domain")
include("application")
include("adapters:inbound-grpc")
include("adapters:outbound-kernel")
include("bootstrap")
include("architecture-tests")
