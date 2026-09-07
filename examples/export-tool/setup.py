from setuptools import find_packages, setup

setup(
    name="syntra-export",
    version="0.1.0",
    description=(
        "Export a Syntra capsule's current learned state to a portable JSON "
        "snapshot for archival, migration, or offline policy evaluation."
    ),
    long_description=open("README.md", encoding="utf-8").read(),
    long_description_content_type="text/markdown",
    packages=find_packages(exclude=("tests", "tests.*")),
    python_requires=">=3.9",
    install_requires=[],  # stdlib only
    entry_points={
        "console_scripts": [
            "syntra-export=export:main",
        ],
    },
    classifiers=[
        "License :: OSI Approved :: Apache Software License",
        "Programming Language :: Python :: 3",
        "Topic :: Scientific/Engineering :: Artificial Intelligence",
    ],
    license="Apache-2.0",
)
