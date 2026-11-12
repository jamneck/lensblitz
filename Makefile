# LensBlitz — common tasks. `make help` lists them.

PREFIX ?= /usr/local
APPDIR ?= /Applications
LSREGISTER := /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister

.PHONY: help build app universal install uninstall test icon clean

help:
	@echo "make build      release binary            -> target/release/lensblitz"
	@echo "make app        app bundle for this Mac   -> dist/LensBlitz.app"
	@echo "make universal  app bundle, arm64 + x86_64 -> dist/LensBlitz.app"
	@echo "make install    copy the app to $(APPDIR) and link the CLI into $(PREFIX)/bin"
	@echo "make uninstall  remove both"
	@echo "make test       run the unit tests"
	@echo "make icon       regenerate the icon from assets/icon/icon.svg"

build:
	cargo build --release --locked

app:
	scripts/bundle.sh

universal:
	scripts/bundle.sh --universal

# Installs whatever `make app` or `make universal` built (building for this Mac if neither ran).
install:
	@test -d dist/LensBlitz.app || scripts/bundle.sh
	rm -rf "$(APPDIR)/LensBlitz.app"
	cp -R dist/LensBlitz.app "$(APPDIR)/LensBlitz.app"
	$(LSREGISTER) -f "$(APPDIR)/LensBlitz.app"
	mkdir -p "$(PREFIX)/bin"
	ln -sf "$(APPDIR)/LensBlitz.app/Contents/MacOS/lensblitz" "$(PREFIX)/bin/lensblitz"
	@echo "Installed. To make LensBlitz the default for .md files, see README.md."

uninstall:
	rm -rf "$(APPDIR)/LensBlitz.app"
	rm -f "$(PREFIX)/bin/lensblitz"

test:
	cargo test --locked

icon:
	scripts/make-icon.sh

clean:
	cargo clean
	rm -rf dist
