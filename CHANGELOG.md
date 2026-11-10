# Changelog

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
