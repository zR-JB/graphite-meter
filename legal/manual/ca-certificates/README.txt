The final scratch image copies /etc/ssl/certs/ca-certificates.crt from the
same Debian-based Go builder that supplies this metadata. The Dockerfile
must copy /usr/share/doc/ca-certificates/copyright from that builder without
rewriting it. This record identifies the package and the exact build-time
source; the final image carries the package copyright file verbatim.

Package: ca-certificates
License basis: Debian package copyright file, including the licenses of the
Mozilla certificate data and Debian packaging sources.
Source: https://salsa.debian.org/debian/ca-certificates
Reference package metadata: https://packages.debian.org/source/stable/ca-certificates
