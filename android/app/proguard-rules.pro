# R8 rules for the release APK.
#
# The Rust core reaches into Java two ways, both of them by name, so R8 may not
# rename or drop either side:
#
#   * `Java_io_github_yjmthu_thunet_Native_*` exports: the symbol names carry the
#     class and method, so `Native` has to keep both.
#   * `JNIEnv::call_method(activity, "setMode", ...)` in `src/android_jni.rs`:
#     the method name is a string in Rust, which no compiler can see.
#
# The package holds a handful of classes, so keeping all of them costs nothing;
# what R8 actually removes is AndroidX, Room and Kotlin code the app never calls.
-keep class io.github.yjmthu.thunet.** { *; }

# Anything else exposing a native method has the same problem.
-keepclasseswithmembernames class * {
    native <methods>;
}

# WorkManager's Room database is reached through its generated implementation,
# which R8 cannot see from the builder call.
-keep class androidx.work.impl.WorkDatabase_Impl { *; }
-keep class * extends androidx.room.RoomDatabase { <init>(); }
-dontwarn androidx.room.paging.**
