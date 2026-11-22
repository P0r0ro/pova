# COPR "make srpm" hook: COPR clones the repo and runs `make -f .copr/Makefile srpm`.
srpm:
	dnf -y install cargo rust rpm-build git tar gzip
	git config --global --add safe.directory '*'
	./packaging/copr/make-srpm.sh "$(outdir)"
