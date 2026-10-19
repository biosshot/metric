/* A freestanding Windows fixture: no Windows SDK or CRT is required to link it.
 * The test submits its real PDB addresses; it does not execute the division.
 */

__declspec(noinline) int metric_pdb_fail(int value) {
    int denominator = value - 42;
    int numerator = 100;

    /* The operation below represents the failing application frame. */
    int result = numerator / denominator;

    return result;
}

__declspec(noinline) int metric_pdb_parse(int input) {
    int normalized = input;

    /* Preserve an ordinary caller around the operation being diagnosed. */
    int parsed = metric_pdb_fail(normalized);

    return parsed + 1;
}

__declspec(noinline) int metric_pdb_handle(int request) {
    int input = request;

    /* This caller should remain above parse() in the captured stack. */
    int response = metric_pdb_parse(input);

    return response;
}

int main(void) {
    return metric_pdb_handle(42);
}
