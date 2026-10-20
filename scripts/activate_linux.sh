#!/usr/bin/env bash
# Activation script for the Linux packaging environment.
#
# The GTK development files come from the pixi environment itself (see
# `[feature.package.target.linux-64.dependencies]`), but conda's pkg-config does
# not add `$CONDA_PREFIX/lib/pkgconfig` to its search path. Without this the
# `glib-sys` / `gtk-sys` build scripts fail with "glib-2.0 was not found".
#
# Setting it here (instead of inside a single task) keeps the variable stable
# across every build in the environment, so cargo does not consider the
# `rerun-if-env-changed=PKG_CONFIG_PATH` build scripts dirty and relink the
# binary behind our back (which would re-add the rpath that `patchelf` strips).

case ":${PKG_CONFIG_PATH:-}:" in
  *":$CONDA_PREFIX/lib/pkgconfig:"*) ;;
  *) export PKG_CONFIG_PATH="$CONDA_PREFIX/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}" ;;
esac
