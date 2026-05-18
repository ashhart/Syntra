from setuptools import find_packages, setup

setup(
    name="syntra-ope",
    version="0.1.0",
    description=(
        "Offline Policy Evaluation for Syntra capsules: "
        "IPS and Doubly Robust estimators with bootstrap confidence intervals"
    ),
    long_description=open("README.md", encoding="utf-8").read(),
    long_description_content_type="text/markdown",
    packages=find_packages(exclude=("tests", "tests.*")),
    python_requires=">=3.9",
    install_requires=[],  # stdlib only
    entry_points={
        "console_scripts": [
            "syntra-ope=evaluate:main",
        ],
    },
    classifiers=[
        "License :: OSI Approved :: Apache Software License",
        "Programming Language :: Python :: 3",
        "Topic :: Scientific/Engineering :: Artificial Intelligence",
    ],
    license="Apache-2.0",
)
