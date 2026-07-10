package io.github.jark006.freezeit.hook;

import androidx.annotation.NonNull;

import io.github.libxposed.api.XposedModule;

public class ModernHook extends XposedModule {
    @Override
    public void onPackageReady(@NonNull PackageReadyParam param) {
        String packageName = param.getPackageName();
        if (!Enum.Package.self.equals(packageName)
                && !Enum.Package.powerkeeper.equals(packageName)
                && !Enum.Package.oplusAthena.equals(packageName)) {
            return;
        }
        HookHealthRegistry.beginScope("package:" + packageName, packageName);
        ModernXposedBackend backend = new ModernXposedBackend(this);
        XpUtils.setHookBackend(backend);
        backend.logFramework("Freezeit modern package hook: " + packageName);
        FreezeitHookEntry.handlePackage(packageName, param.getClassLoader());
        backend.logFramework("Freezeit hook health: " + HookHealthRegistry.toJson());
    }

    @Override
    public void onSystemServerStarting(@NonNull SystemServerStartingParam param) {
        HookHealthRegistry.beginScope("system_server", "system_server");
        ModernXposedBackend backend = new ModernXposedBackend(this);
        XpUtils.setHookBackend(backend);
        backend.logFramework("Freezeit modern system_server hook");
        FreezeitHookEntry.hookAndroid(param.getClassLoader());
        backend.logFramework("Freezeit hook health: " + HookHealthRegistry.toJson());
    }
}
