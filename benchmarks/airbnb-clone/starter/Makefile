.PHONY: run stop seed check-ready

FIREBASE_PORT ?= 8080
NEXT_PORT ?= 3000
PROJECT_ID ?= airbnb-clone-dev

run:
	@bash -c '\
		firebase emulators:start --project $(PROJECT_ID) 2>&1 & \
		EMUL_PID=$$!; \
		echo "Waiting for Firestore emulator on :$(FIREBASE_PORT)..."; \
		timeout=60; \
		while ! curl -sf http://localhost:$(FIREBASE_PORT) >/dev/null 2>&1; do \
			timeout=$$((timeout - 2)); \
			if [ $$timeout -le 0 ]; then echo "Emulator timeout"; kill $$EMUL_PID 2>/dev/null; exit 1; fi; \
			sleep 2; \
		done; \
		echo "Emulators ready. Seeding..."; \
		npx ts-node scripts/seed.ts 2>/dev/null || true; \
		echo "Starting Next.js on :$(NEXT_PORT)..."; \
		pnpm dev 2>&1 & \
		timeout=60; \
		while ! curl -sf http://localhost:$(NEXT_PORT) >/dev/null 2>&1; do \
			timeout=$$((timeout - 2)); \
			if [ $$timeout -le 0 ]; then echo "Next.js timeout"; exit 1; fi; \
			sleep 2; \
		done; \
		echo "READY"; \
		wait $$EMUL_PID \
	'

stop:
	@pkill -f "firebase emulators" 2>/dev/null || true
	@pkill -f "next dev" 2>/dev/null || true
	@echo "Stopped"

seed:
	npx ts-node scripts/seed.ts

check-ready:
	@curl -sf http://localhost:$(FIREBASE_PORT) >/dev/null && echo "Firestore: OK" || echo "Firestore: NOT READY"
	@curl -sf http://localhost:$(NEXT_PORT) >/dev/null && echo "Next.js: OK" || echo "Next.js: NOT READY"
