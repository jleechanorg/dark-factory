.PHONY: build test run seed validate-size

build:
	npm install

test:
	npm test

seed:
	node -e "require('./src/models/db').resetDB()"

validate-size:
	python3 scripts/validate-size.py

run:
	npx firebase emulators:exec --only firestore,auth "npm start"
