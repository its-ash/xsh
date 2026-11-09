.PHONY: run build deploy

LLM_URL ?= http://127.0.0.1:9694/v1
LLM_MODEL ?= router/auto

run:
	sh app/bundle.sh debug
	open -n -W target/xsh.app

build:
	sh app/bundle.sh release

# Builds the release itself, after it sets the new version.
deploy:
	VERSION="$(VERSION)" LLM_URL="$(LLM_URL)" LLM_MODEL="$(LLM_MODEL)" sh scripts/deploy.sh
