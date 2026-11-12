/*
 * deploy/nests/buildd/scanners/pigeonpost.yar — the analysis plane's own YARA rules, compiled
 * together with the YARA-Forge core feed by nestscan-update. Any match blocks a release, so each
 * rule here is narrow: strings no ordinary Nest has a reason to carry.
 */

rule PIGEONPOST_Cryptominer
{
    meta:
        description = "Mining pool protocol or a known miner's options"
        author = "Pigeonpost"
    strings:
        $pool1 = "stratum+tcp://" ascii wide nocase
        $pool2 = "stratum+ssl://" ascii wide nocase
        $pool3 = "stratum2+tcp://" ascii wide nocase
        $opt1 = "--donate-level" ascii
        $opt2 = "\"donate-level\"" ascii
        $algo1 = "cryptonight" ascii nocase
        $algo2 = "randomx" ascii nocase fullword
        $name = "xmrig" ascii nocase fullword
    condition:
        any of ($pool*) or (2 of ($opt*, $algo*, $name))
}

rule PIGEONPOST_ReverseShell
{
    meta:
        description = "Shell redirected over a socket"
        author = "Pigeonpost"
    strings:
        $a = "/dev/tcp/" ascii
        $b = /bash -i >& ?\/dev\// ascii
        $c = /nc(at)? (-[a-z]+ )*-e \/bin\/(ba)?sh/ ascii
        $d = "socat exec:" ascii nocase
        $e = "pty.spawn(\"/bin/sh\")" ascii
        $f = "pty.spawn('/bin/sh')" ascii
    condition:
        any of them
}

rule PIGEONPOST_TestSignature
{
    meta:
        description = "The EICAR anti-malware test file (for the runbook's smoke test)"
        author = "Pigeonpost"
    strings:
        // "EICAR-STANDARD-ANTIVIRUS-TEST-FILE", in hex so this file is not itself a test file.
        $eicar = { 45 49 43 41 52 2D 53 54 41 4E 44 41 52 44 2D 41 4E 54 49 56 49 52 55 53 2D 54 45 53 54 2D 46 49 4C 45 }
    condition:
        $eicar
}
