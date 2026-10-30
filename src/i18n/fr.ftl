# Ce qu'un satellite dit aux gens, en français. Phrases courtes, sans tiret cadratin.

pairing-code-shown = Pour appairer cet appareil, saisissez ce code dans Nestor :
# Dit à voix haute, chiffre par chiffre. La voix fournie joue un extrait enregistré pour chaque texte entre
# les chiffres (voice/, enregistré par scripts/voice.py de nestor-satellite-python) : le réenregistrer après une modification.
pairing-code-spoken = Code d'appairage : { $digits }. Je répète : { $digits }.
pairing-headless = Cet appareil n'a ni écran ni voix : pour l'appairer, créez un code d'enrôlement dans Nestor et renseignez NESTOR_PAIRING_URI.
pairing-paired = Appairé avec Nestor.
pairing-expired = Le code d'appairage a expiré. En voici un nouveau.
pairing-refused = { $reason ->
        [expired] Ce code d'enrôlement a expiré.
        [used] Ce code d'enrôlement a déjà servi.
        [cancelled] Ce code d'enrôlement a été annulé.
       *[unknown] Ce code d'enrôlement n'est pas valide.
    } Utilisez plutôt le code de l'appareil.
pairing-already-refused = Ce code d'enrôlement a déjà été refusé.
pairing-rate-limited = Trop de codes erronés. Nouvel essai dans { $seconds } s.
pairing-busy = Trop d'appareils attendent d'être appairés. Nouvel essai dans { $seconds } s.
identify-requested = Nestor demande à cet appareil de se signaler.

error-protocol-unsupported = Nestor ne parle pas le protocole de cet appareil : mettez à jour l'appareil ou Nestor.
error-pairing-refused = Nestor a refusé la demande d'appairage : mettez à jour l'appareil.
error-not-pairing = Cet appareil n'attend pas de code.
error-method-not-declared = Cet appareil n'accepte pas de code d'enrôlement de cette façon.
error-enrollment-code = Un code d'enrôlement compte 8 lettres et chiffres, comme K7QF-2M9X.
error-other-core = Cette URI d'appairage est celle d'un autre Nestor.
error-token-refused = Nestor ne connaît plus cet appareil : appairez-le à nouveau.
error-unreachable = Nestor est injoignable.
