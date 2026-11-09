.PHONY: run build deploy

LLM_URL ?= http://127.0.0.1:9694/v1
LLM_MODEL ?= router/auto

run:
	sh app/bundle.sh debug
	open -n -W target/xsh.app

build:
	sh app/bundle.sh release

# Bumps the version, runs `make build`, commits with a message from the local
# model at LLM_URL, pushes to main, and creates a GitHub release with gh.
deploy:
	VERSION="$(VERSION)" LLM_URL="$(LLM_URL)" LLM_MODEL="$(LLM_MODEL)" sh scripts/deploy.sh
