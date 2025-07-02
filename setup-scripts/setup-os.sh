#!/bin/bash

# Install Python 3.9.15 using pyenv, if not already installed
# pyenv install -s 3.9.15
# pyenv local 3.9.15
# Set the Python version and create a virtual environment
PYENV_VERSION=3.9.15 python3 -m venv snos-env

# Activate the virtual environment and install the dependencies
source snos-env/bin/activate
pip install -r requirements.txt


# Change the version accordingly in requirement.txt

# CAIRO_VER="0.13.3"
# CAIRO_LANG_COMMIT="8e11b8cc65ae1d0959328b1b4a40b92df8b58595"

# v0.13.2.1
CAIRO_VER="0.13.2"
CAIRO_LANG_COMMIT="a86e92bfde9c171c0856d7b46580c66e004922f3"

# CAIRO_VER="0.13.2"
# CAIRO_LANG_COMMIT="4ea4fe8e167845a3402ae2ea0a8b6004aad18dd5"

if ! command -v cairo-compile >/dev/null; then
    echo "please start cairo($CAIRO_VER) dev environment"
    exit 1
fi

if ! command -v starknet-compile-deprecated >/dev/null; then
    echo "please start cairo($CAIRO_VER) dev environment"
    exit 1
fi

echo -e "\ninitializing cairo-lang($CAIRO_VER)...\n"

git submodule update --init --recursive

cd cairo-lang

git checkout $CAIRO_LANG_COMMIT
cd ..

FETCHED_CAIRO_VER="$(cat cairo-lang/src/starkware/cairo/lang/VERSION)"

if [ "$CAIRO_VER" != "$FETCHED_CAIRO_VER" ]; then
    echo "incorrect cairo ver($FETCHED_CAIRO_VER) expecting $CAIRO_VER"
    exit 1
fi

echo "deleting old OS"
rm -rf build/os_v_$CAIRO_VER.json

echo -e "creating os_v_$CAIRO_VER.json \n"

cairo-compile cairo-lang/src/starkware/starknet/core/os/os.cairo --output build/os_v_$CAIRO_VER.json --cairo_path cairo-lang/src
