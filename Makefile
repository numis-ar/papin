# Thin orchestration over the per-language builds.

.PHONY: rust-test rust-build deb android-build docs

rust-test:
	$(MAKE) -C rust test

rust-build:
	$(MAKE) -C rust build

deb:
	$(MAKE) -C rust deb

android-build:
	@if [ ! -f android/gradle/wrapper/gradle-wrapper.jar ]; then \
		echo "error: gradle-wrapper.jar not present — run 'gradle wrapper --gradle-version 8.9' in android/ first (see android/README.md)"; \
		exit 1; \
	fi
	cd android && ./gradlew build

docs:
	@echo "docs live in docs/ — edit in place; nothing to generate"
