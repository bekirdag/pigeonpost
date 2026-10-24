# Kotlin serialization generates serializers; its consumer rules keep those entry points.
# AppAuth also supplies its own consumer rules. Do not keep the whole application.

# Group encryption: the UniFFI binding and JNA find classes, fields and callbacks by name.
-keep class dev.pigeonpost.groupcrypto.** { *; }
-keep class com.sun.jna.** { *; }
-keepclassmembers class * extends com.sun.jna.** { public *; }
-dontwarn java.awt.**
-dontwarn com.sun.jna.**
