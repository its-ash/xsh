.PHONY: run build install uninstall deploy theme

LLM_URL ?= http://127.0.0.1:9694/v1
LLM_MODEL ?= router/auto
APP_DIR ?= /Applications
BIN_DIR ?= /usr/local/bin

run:
	sh app/bundle.sh debug
	open -n -W target/xsh.app

build:
	sh app/bundle.sh release

# Builds the release app, copies it to APP_DIR, and links BIN_DIR/xsh to the
# `xsh` inside it. Uses sudo only when BIN_DIR is not writable.
install: build
	APP_DIR="$(APP_DIR)" BIN_DIR="$(BIN_DIR)" sh scripts/install.sh install

# Removes the app and the `xsh` link. Settings, history, and bundles stay.
uninstall:
	APP_DIR="$(APP_DIR)" BIN_DIR="$(BIN_DIR)" sh scripts/install.sh uninstall

# Opens the theme editor in the browser: a preview of terminal output where
# every color can be changed, exported as a theme file.
theme:
	mkdir -p target/theme-editor
	cp scripts/theme-editor.html target/theme-editor/index.html
	sh scripts/theme-presets.sh > target/theme-editor/themes.js
	open target/theme-editor/index.html

# Bumps the version, runs `make build`, commits with a message from the local
# model at LLM_URL, pushes to main, and creates a GitHub release with gh.
deploy:
	VERSION="$(VERSION)" LLM_URL="$(LLM_URL)" LLM_MODEL="$(LLM_MODEL)" sh scripts/deploy.sh
