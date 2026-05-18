from setuptools import find_packages, setup

setup(
    name="syntra-fraud",
    version="0.1.0",
    description="Syntra-driven adaptive fraud threshold selection for transaction scoring",
    packages=find_packages(exclude=("tests", "tests.*")),
    install_requires=["requests>=2.28"],
    python_requires=">=3.9",
)
