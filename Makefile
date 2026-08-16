.PHONY: bootstrap check test smoke apple-project android docker

bootstrap:
	./scripts/bootstrap.sh

check:
	./scripts/check.sh

test:
	cargo test --workspace --all-features

smoke:
	./scripts/smoke.sh

apple-project:
	cd apps/apple && xcodegen generate

android:
	./scripts/check-android.sh

docker:
	docker build -f packaging/docker/Dockerfile -t covalent:local .
