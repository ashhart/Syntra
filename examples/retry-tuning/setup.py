from setuptools import find_packages, setup

setup(
    name="syntra-retry",
    version="0.1.0",
    description="Syntra-driven adaptive retry policies for HTTP clients",
    packages=find_packages(exclude=("tests", "tests.*")),
    install_requires=["requests>=2.28"],
    python_requires=">=3.9",
)
