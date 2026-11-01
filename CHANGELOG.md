# Changelog

## [0.4.0](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.3.0...v0.4.0) (2026-11-01)


### ⚠ BREAKING CHANGES

* the default core URL uses port 8471.

### Bug Fixes

* use the core's port 8471 ([#14](https://github.com/nestor-home/nestor-satellite-rust/issues/14)) ([8ddf19a](https://github.com/nestor-home/nestor-satellite-rust/commit/8ddf19ab75d7ff643ee39ed264347e2879818166))

## [0.3.0](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.2.0...v0.3.0) (2026-11-01)


### ⚠ BREAKING CHANGES

* the SDK announces `nestor.satellite/1`; cores before nestor-home 0.2.0 refuse it, and devices pair again with a 0.2.0 core.

### Features

* speak nestor.satellite/1 ([#11](https://github.com/nestor-home/nestor-satellite-rust/issues/11)) ([eb0048c](https://github.com/nestor-home/nestor-satellite-rust/commit/eb0048c19171c72ede440979b272d76df8dd09e4))


### Bug Fixes

* move public URLs to nestorhome.dev ([#6](https://github.com/nestor-home/nestor-satellite-rust/issues/6)) ([319749e](https://github.com/nestor-home/nestor-satellite-rust/commit/319749e055482703bb2745381f45ded38cc15078))

## [0.2.0](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.1.0...v0.2.0) (2026-11-01)


### Features

* ship the CLI as prebuilt binaries ([#4](https://github.com/nestor-home/nestor-satellite-rust/issues/4)) ([957e63a](https://github.com/nestor-home/nestor-satellite-rust/commit/957e63aa0d836908cf6219e4478a25b61693541a))
