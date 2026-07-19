<#
.SYNOPSIS
  Fetch CC0 PBR texture sets from ambientCG into assets/textures/.

.DESCRIPTION
  Idempotent. For each set below it downloads the 2K-JPG zip from ambientCG
  (all content is CC0), extracts only the Color / NormalGL / Roughness /
  AmbientOcclusion maps, and renames them to a standard scheme:

      albedo.jpg  normal.jpg  roughness.jpg  ao.jpg   (ao skipped if absent)

  into the per-layer / per-species target folders.

  A set whose target folder already has albedo.jpg is SKIPPED (re-run safe).
  If the 2K zip fails, the script retries once with the 1K variant.

  Run:  pwsh -File tools/fetch_textures.ps1
#>

$ErrorActionPreference = 'Stop'

# repo-root/assets/textures  (this script lives in repo-root/tools)
$Root       = Split-Path -Parent $PSScriptRoot
$TexRoot    = Join-Path $Root 'assets\textures'
$TmpRoot    = Join-Path ([System.IO.Path]::GetTempPath()) 'ambientcg_fetch'

# assetId -> target subfolder (relative to assets/textures)
# birch bark: ambientCG has NO white birch bark material (only finished birch
# countertop wood) -> deliberately omitted, reported by the caller.
$Sets = @(
    @{ Id = 'Grass001';  Dest = 'ground\grass'        }  # natural fresh green lawn/meadow
    @{ Id = 'Ground023'; Dest = 'ground\forest_floor' }  # brown forest leaf-litter (dirt/leaves/sticks)
    @{ Id = 'Rock035';   Dest = 'ground\rock'         }  # grey layered/fractured cliff rock
    @{ Id = 'Ground081'; Dest = 'ground\dirt'         }  # brown bare dirt path, rocky/gravel
    @{ Id = 'Bark014';   Dest = 'bark\pine'           }  # rough conifer (fir) brown plated bark
    @{ Id = 'Bark012';   Dest = 'bark\broadleaf'      }  # oak: grey-brown broadleaf bark (beech-like)
)

# ambientCG map suffix -> our standard filename
$MapMap = [ordered]@{
    'Color'            = 'albedo.jpg'
    'NormalGL'         = 'normal.jpg'
    'Roughness'        = 'roughness.jpg'
    'AmbientOcclusion' = 'ao.jpg'
}

New-Item -ItemType Directory -Force -Path $TmpRoot | Out-Null

function Get-Set {
    param([string]$Id, [string]$Dest)

    $target = Join-Path $TexRoot $Dest
    New-Item -ItemType Directory -Force -Path $target | Out-Null

    if (Test-Path (Join-Path $target 'albedo.jpg')) {
        Write-Host "SKIP  $Id -> $Dest (albedo.jpg already present)"
        return
    }

    foreach ($variant in @('2K-JPG', '1K-JPG')) {
        $zip     = Join-Path $TmpRoot "$Id`_$variant.zip"
        $extract = Join-Path $TmpRoot "$Id`_$variant"
        $url     = "https://ambientcg.com/get?file=$Id`_$variant.zip"

        Write-Host "FETCH $Id ($variant) ..."
        try {
            & curl.exe -sL --fail -o $zip $url
            if ($LASTEXITCODE -ne 0) { throw "curl exit $LASTEXITCODE" }
            if (-not (Test-Path $zip) -or (Get-Item $zip).Length -lt 10000) {
                throw "download too small / missing"
            }

            if (Test-Path $extract) { Remove-Item -Recurse -Force $extract }
            Expand-Archive -Path $zip -DestinationPath $extract -Force

            $copied = 0
            foreach ($suffix in $MapMap.Keys) {
                $src = Get-ChildItem -Path $extract -Filter "*_$suffix.jpg" -File |
                       Select-Object -First 1
                if ($src) {
                    Copy-Item $src.FullName (Join-Path $target $MapMap[$suffix]) -Force
                    $copied++
                }
            }

            if (-not (Test-Path (Join-Path $target 'albedo.jpg'))) {
                throw "no Color map found in archive"
            }
            Write-Host "  OK  $Id ($variant): $copied maps -> $Dest"
            return
        }
        catch {
            Write-Warning "  FAIL $Id ($variant): $($_.Exception.Message)"
            if ($variant -eq '1K-JPG') {
                Write-Warning "  GIVING UP on $Id (both 2K and 1K failed)"
            } else {
                Write-Warning "  retrying with 1K variant ..."
            }
        }
    }
}

foreach ($s in $Sets) { Get-Set -Id $s.Id -Dest $s.Dest }

Write-Host ''
Write-Host '=== Result ==='
Get-ChildItem -Path $TexRoot -Recurse -File -Filter '*.jpg' |
    Sort-Object FullName |
    ForEach-Object {
        $rel = $_.FullName.Substring($TexRoot.Length + 1)
        '{0,-45} {1,10:N0} bytes' -f $rel, $_.Length
    }
