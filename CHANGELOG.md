# Changelog

## [0.4.2](https://github.com/scottmckendry/haunt/compare/haunt-v0.4.1...haunt-v0.4.2) (2026-11-14)


### Bug Fixes

* cwd lookup edgecase ([736beb5](https://github.com/scottmckendry/haunt/commit/736beb529a6384b3be97a28d086eb5dcc7af586c))
* improve frame timing ([5b8921d](https://github.com/scottmckendry/haunt/commit/5b8921d9a2b59603913b5ce8463782b393768cc5))
* watch child exit instead of relying on pty eof ([64f8db5](https://github.com/scottmckendry/haunt/commit/64f8db5bd4bd984a3f64d043201effa30a72409a))


### Performance Improvements

* avoid heap allocations in render loop ([9f5750a](https://github.com/scottmckendry/haunt/commit/9f5750abe5c99fbbb2ffaba7866181072f8610a9))
* reduce syscalls per frame ([07631a6](https://github.com/scottmckendry/haunt/commit/07631a68e523d545a8f4db65ae65dba0ad9829e9))

## [0.4.1](https://github.com/scottmckendry/haunt/compare/haunt-v0.4.0...haunt-v0.4.1) (2026-11-13)


### Bug Fixes

* **config:** include ghostty theme in reload ([adc913b](https://github.com/scottmckendry/haunt/commit/adc913b3e1e14bdea65447ef97c00d15f5a00baf))
* enforce minimum pane width ([91a36fe](https://github.com/scottmckendry/haunt/commit/91a36fe2be8b7c5348c66569e0c2b622fdfc60d4))
* **input:** cursor up/down on scroll in alternate screen apps ([599c35f](https://github.com/scottmckendry/haunt/commit/599c35f1220d9424babe59f5da78d3f02037fed2))
* remove 100ms blocking sleep on shutdown ([3b8332c](https://github.com/scottmckendry/haunt/commit/3b8332c717afd8b06992364afcda8767eaafd709))


### Performance Improvements

* drop frame pacing in favour damage-tracking ([f96191b](https://github.com/scottmckendry/haunt/commit/f96191b6754c8040ddddbbef01bdcb35e7e9e630))
* optimise gpu rendering ([28dc764](https://github.com/scottmckendry/haunt/commit/28dc764b4accfc688c2fe6f8cf2fbd0690d98853))

## [0.4.0](https://github.com/scottmckendry/haunt/compare/haunt-v0.3.1...haunt-v0.4.0) (2026-11-12)


### ⚠ BREAKING CHANGES

* **config:** use named tables for apps

### Features

* add launch message and mascot ([12d6963](https://github.com/scottmckendry/haunt/commit/12d69633222d3fa1e7cd812e44334fb2fc8675ff))
* **banner:** more minimal with no cpu/mem details ([e0a45a5](https://github.com/scottmckendry/haunt/commit/e0a45a519581e8a9790eadf5eb0be6bca2fa0897))
* resize panes with click-drag ([8a07f43](https://github.com/scottmckendry/haunt/commit/8a07f430f9d4ba6838b124a7fd613e45ced3e2af))


### Bug Fixes

* **banner:** ghost persists into tui apps ([9899c15](https://github.com/scottmckendry/haunt/commit/9899c1569063b7266a7389af2f61b7a9faf45470))
* **graphics:** invalid image placement/offsets ([178717b](https://github.com/scottmckendry/haunt/commit/178717bebe637130ac7a2a35d96507d05f91bd4a))
* unnecessary cast error ([b6353e1](https://github.com/scottmckendry/haunt/commit/b6353e116bbea142c9418d27896d35b48617754d))


### Code Refactoring

* **config:** use named tables for apps ([5a3cca1](https://github.com/scottmckendry/haunt/commit/5a3cca16c2fe1e360c8ff3a346c6f79d897b5268))

## [0.3.1](https://github.com/scottmckendry/haunt/compare/haunt-v0.3.0...haunt-v0.3.1) (2026-11-11)


### Bug Fixes

* child process termination on shutdown ([864ad65](https://github.com/scottmckendry/haunt/commit/864ad651ba00232eae3652ac06e4a2c0fe7bb1d5))
* mouse click-drag missing final event to pane ([4220258](https://github.com/scottmckendry/haunt/commit/42202581c19d9380db7e991de50532033f055fd2))
* use bright black for separator chars ([a0fe0ae](https://github.com/scottmckendry/haunt/commit/a0fe0ae9ee45c149b31ddb6973ec69556d5c8594))


### Performance Improvements

* remove per-cell allocations, batch ffi calls ([4f68056](https://github.com/scottmckendry/haunt/commit/4f6805691da72a7b172b6942cd44c4fcaa53abed))

## [0.3.0](https://github.com/scottmckendry/haunt/compare/haunt-v0.2.0...haunt-v0.3.0) (2026-11-10)


### Features

* experimental multiarch support ([927065e](https://github.com/scottmckendry/haunt/commit/927065edec571268d4ceca6fa740d60afd13d2ae))


### Bug Fixes

* **macos:** handle 'option' bindings ([430f7b1](https://github.com/scottmckendry/haunt/commit/430f7b1d1afddf38253525a003901f4b322296c8))

## [0.2.0](https://github.com/scottmckendry/haunt/compare/haunt-v0.1.0...haunt-v0.2.0) (2026-11-10)


### Features

* **config:** add default reload binding ([d6d053d](https://github.com/scottmckendry/haunt/commit/d6d053dc5754b87babda997fd0210361b3ac5e84))
* **overlay:** add module, help overlay ([2cbc701](https://github.com/scottmckendry/haunt/commit/2cbc701952261446921e6ce289502188da95097c))
* **overlay:** animate notifications with shared type ([b307c48](https://github.com/scottmckendry/haunt/commit/b307c48d4642cdc79397d302cfdc04ac9dc5d400))
* **paint:** reload notifications ([5cdf96b](https://github.com/scottmckendry/haunt/commit/5cdf96b65360f20f3ca11db7c06a9750cac27a4a))


### Bug Fixes

* avoid exit on invalid config ([b3f56ee](https://github.com/scottmckendry/haunt/commit/b3f56ee986e6bae28a9d224983cd3ed815e7e4ee))
* pane resize lag in some apps ([12dab3e](https://github.com/scottmckendry/haunt/commit/12dab3e7a0189434784c30d31d9181608b184e8c))

## [0.1.0](https://github.com/scottmckendry/haunt/compare/haunt-v0.1.0...haunt-v0.1.0) (2026-11-09)


### Features

* basic pty shell with libghostty-vt ([1bd6083](https://github.com/scottmckendry/haunt/commit/1bd6083e9e2b102ebd1dc4161aa92212d88a90b1))
* configurable split separator ([be0c795](https://github.com/scottmckendry/haunt/commit/be0c7952e8f666ab0a581d954e8ffd7525380c7f))
* expand osc support ([2fb6760](https://github.com/scottmckendry/haunt/commit/2fb6760539040f81dfc165cd915bcd6fc63ef199))
* forward titles ([f2d9c60](https://github.com/scottmckendry/haunt/commit/f2d9c60ee461d3a3d6a28a544ab1628122301d9e))
* improve input handling ([b5ee752](https://github.com/scottmckendry/haunt/commit/b5ee75269cd4d6c3dd70badae92103ccbefbb397))
* improve selection and mouse handling ([79c383c](https://github.com/scottmckendry/haunt/commit/79c383cb3e94c93e4053b47816280d2b48b7c533))
* inherit ghostty theme ([10f1ba6](https://github.com/scottmckendry/haunt/commit/10f1ba6a3b833a81a2cd106f694473e341ad25fd))
* init haunt with flake and cargo ([4de1414](https://github.com/scottmckendry/haunt/commit/4de14142b5f75db9a4ab02da5ef426a06c0c302c))
* mouse input support ([2e06365](https://github.com/scottmckendry/haunt/commit/2e06365b87b10b45dcab5e5dc75c768ce70a969f))
* proc cleanup on exit ([6876b86](https://github.com/scottmckendry/haunt/commit/6876b8606ed4a30012e9c5783d16300349dd5f85))
* support for kitty graphics protocol ([2a0b426](https://github.com/scottmckendry/haunt/commit/2a0b4262ef384893f2b27266971dea5889f0635f))
* toml configuration support ([a8a536f](https://github.com/scottmckendry/haunt/commit/a8a536ff1df5d12fbec862289b3431d079e879ec))


### Bug Fixes

* **config:** handle edge cases ([d9e7dcd](https://github.com/scottmckendry/haunt/commit/d9e7dcd3b9b5e53d93f226b51381d9445ad0da31))
* default configuration file ([cabaf1f](https://github.com/scottmckendry/haunt/commit/cabaf1fbcb1eefdde389f1023132b8390b6b2caf))
* forward cwd to new tabs ([227e897](https://github.com/scottmckendry/haunt/commit/227e8976eeacddac61f491483b6c2019ef513dbc))
* **paint:** redundant blank cells after emoji ([cee913f](https://github.com/scottmckendry/haunt/commit/cee913fd6d3bf23b272614840fcef992f7121d4d))


### Performance Improvements

* regular cells skip grapheme width calc ([4f13bca](https://github.com/scottmckendry/haunt/commit/4f13bca61b9214728f95f9ac2ad041c8e09001d3))
* reuse grapheme buffers ([acfc249](https://github.com/scottmckendry/haunt/commit/acfc2490620376fd042cfe9338be28a1b1247481))


### Documentation

* add demo video ([6d62bf2](https://github.com/scottmckendry/haunt/commit/6d62bf29c0aefee953025c3fc178220aac8af2f7))
