# Causewaybay Code. Each implementation lives in a folder of its own; today there is one,
# in Rust (rust/). Targets given here are passed on to it:
#
#   make build | install | start | stop | status | logs | url | test | test-web | e2e ...
#
# `make help` lists them.

IMPL ?= rust

.DEFAULT_GOAL := help

%:
	@$(MAKE) --no-print-directory -C $(IMPL) $@

# (Not a target to pass on.)
Makefile: ;
