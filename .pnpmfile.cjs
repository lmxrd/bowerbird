// hey-api's generator uses the TypeScript compiler API, which TypeScript 7
// (the native compiler this project type-checks with) no longer has. Give
// it its own copy of the last release that does: 6.x. `tsc` is unaffected.
module.exports = {
  hooks: {
    readPackage(pkg) {
      if (pkg.name === "@hey-api/openapi-ts") {
        if (pkg.peerDependencies) delete pkg.peerDependencies.typescript;
        if (pkg.peerDependenciesMeta) delete pkg.peerDependenciesMeta.typescript;
        pkg.dependencies = { ...pkg.dependencies, typescript: "6.0.3" };
      }
      return pkg;
    },
  },
};
