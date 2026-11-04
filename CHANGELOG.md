# Changelog

## [0.6.0](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.5.2...v0.6.0) (2026-11-04)


### ⚠ BREAKING CHANGES

* name the software in the manifest, and update it on request ([#30](https://github.com/nestor-home/nestor-satellite-rust/issues/30))

### Features

* name the software in the manifest, and update it on request ([#30](https://github.com/nestor-home/nestor-satellite-rust/issues/30)) ([5291253](https://github.com/nestor-home/nestor-satellite-rust/commit/5291253dd56c3ef81f61ad5ea3b9225f68f91174))

## [0.5.2](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.5.1...v0.5.2) (2026-11-04)


### Bug Fixes

* **deps:** update rust crate tokio-tungstenite to 0.30.0 ([#25](https://github.com/nestor-home/nestor-satellite-rust/issues/25)) ([5c84d91](https://github.com/nestor-home/nestor-satellite-rust/commit/5c84d91b8170cfd65c8350c014557b3b3d4e3fb9))

## [0.5.1](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.5.0...v0.5.1) (2026-11-03)


### Bug Fixes

* **deps:** update rust crate reqwest to 0.13.0 ([#22](https://github.com/nestor-home/nestor-satellite-rust/issues/22)) ([5be1981](https://github.com/nestor-home/nestor-satellite-rust/commit/5be1981f96deea2c2d2d0752675d056b4bd68338))

## [0.5.0](https://github.com/nestor-home/nestor-satellite-rust/compare/v0.4.0...v0.5.0) (2026-11-02)


### ⚠ BREAKING CHANGES

* talk over one WebSocket link, with voice as a feature ([#18](https://github.com/nestor-home/nestor-satellite-rust/issues/18))

### Features

* talk over one WebSocket link, with voice as a feature ([#18](https://github.com/nestor-home/nestor-satellite-rust/issues/18)) ([cee3b8e](https://github.com/nestor-home/nestor-satellite-rust/commit/cee3b8e5d8f19010dd120e97df39ccebc4bbe0ad))

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
