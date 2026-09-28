@rem Gradle startup script for Windows (see gradlew / README.md: the wrapper
@rem jar is not committed; run `gradle wrapper --gradle-version 8.9` first).

@echo off
set DIR=%~dp0
set JAR=%DIR%gradle\wrapper\gradle-wrapper.jar
if not exist "%JAR%" (
    echo error: gradle-wrapper.jar not committed; run `gradle wrapper --gradle-version 8.9` first. 1>&2
    exit /b 1
)
java -jar "%JAR%" %*
