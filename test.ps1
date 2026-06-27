# ==============================================================================
# SCRIPT DE TÉLÉCHARGEMENT AUTOMATIQUE MTG (Meteosat 3e Génération)
# ==============================================================================

# Configuration des erreurs : on veut voir s'il y a un problème précis
$ErrorActionPreference = "Stop"

# Étape 1 : Récupération d'un token d'accès tout neuf
Write-Host "1. Demande d'un nouveau Token d'accès..." -ForegroundColor Cyan
try {
    $ReponseToken = Invoke-RestMethod -Method Post -Uri "https://api.eumetsat.int/token" `
         -Credential (New-Object System.Management.Automation.PSCredential($env:Consumer_key, (ConvertTo-SecureString $env:Consumer_secret -AsPlainText -Force))) `
         -Body "grant_type=client_credentials"

    $env:API_token = $ReponseToken.access_token
    Write-Host "-> Token récupéré avec succès." -ForegroundColor Green
} catch {
    Write-Host "Erreur lors de la génération du Token. Vérifie tes clés Consumer Key et Secret." -ForegroundColor Red
    Exit
}

# Étape 2 : Recherche du produit MTG le plus récent disponible
Write-Host "2. Recherche du dernier fichier MTG FCI Level 1C..." -ForegroundColor Cyan
$ResultatRecherche = Invoke-RestMethod -Method Get -Uri "https://api.eumetsat.int/data/search-products/os?format=json&pi=EO:EUM:DAT:0665&count=1" `
     -Headers @{ Authorization = "Bearer $env:API_token" }

# Extraction et nettoyage de l'ID du produit (sélection du premier si liste)
if ($ResultatRecherche.features.id -is [array]) {
    $IdProduit = $ResultatRecherche.features.id[0]
} else {
    $IdProduit = ($ResultatRecherche.features.id -split " ")[0]
}

if (-not $IdProduit) {
    Write-Host "Aucun produit trouvé dans la recherche OpenSearch." -ForegroundColor Red
    Exit
}

Write-Host "-> Produit unique sélectionné : $IdProduit" -ForegroundColor Green

# Étape 3 : Téléchargement du fichier lourd
Write-Host "3. Lancement du téléchargement (Le fichier est très lourd, patience...)" -ForegroundColor Yellow
try {
    # On utilise l'ID unique nettoyé dans l'URL
    Invoke-WebRequest -Method Get -Uri "https://api.eumetsat.int/data/download/collections/EO:EUM:DAT:0665/products/$IdProduit" `
         -Headers @{ Authorization = "Bearer $env:API_token" } `
         -OutFile "image_mtg.nc" `
         -UseBasicParsing

    Write-Host "`n-> Téléchargement terminé avec succès ! Le fichier 'image_mtg.nc' est disponible." -ForegroundColor Green
} catch {
    Write-Host "`nErreur lors du téléchargement. Le fichier est peut-être en cours de génération sur le serveur." -ForegroundColor Red
}