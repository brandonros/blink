/* Double-width compare/exchange: results, register widths and preserved flags. */
typedef unsigned long U;
static long call(long n,long a,long b,long c) {long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c):"rcx","r11","memory");return r;}
#define CHECK(x) do {if(!(x)){call(231,__LINE__,0,0);__builtin_unreachable();}} while(0)
struct pair {U lo,hi;} __attribute__((aligned(16)));
__attribute__((noreturn,force_align_arg_pointer)) void _start(void) {
    for(int wide=0;wide<2;wide++) for(int equal=0;equal<2;equal++) for(int locked=0;locked<2;locked++) {
        static struct pair value; value=(struct pair){0x1122334455667788ul,0x99aabbccddeeff00ul};
        U a=wide?value.lo:0xabcdef0055667788ul,d=wide?value.hi:0x9876540011223344ul;
        if(!equal)a^=1;
        U original_a=a,original_d=d,b=0x123456789abcdef0ul,c=0xfedcba9876543210ul,flags;
#define RUN(ins) __asm__ volatile("pushq $0x897; popfq; " ins "; pushfq; popq %0":"=r"(flags),"+a"(a),"+d"(d),"+m"(value):"b"(b),"c"(c):"cc","memory")
        if(wide){if(locked){RUN("lock cmpxchg16b %3");}else{RUN("cmpxchg16b %3");}}
        else {if(locked){RUN("lock cmpxchg8b %3");}else{RUN("cmpxchg8b %3");}}
        CHECK((flags&0x8d5)==(0x895|(equal?64:0)));
        if(equal){CHECK(a==original_a && d==original_d);CHECK(value.lo==(wide?b:((c&0xffffffff)<<32)|(b&0xffffffff)));CHECK(value.hi==(wide?c:0x99aabbccddeeff00ul));}
        else {CHECK(a==(wide?0x1122334455667788ul:0x55667788ul));CHECK(d==(wide?0x99aabbccddeeff00ul:0x11223344ul));CHECK(value.lo==0x1122334455667788ul && value.hi==0x99aabbccddeeff00ul);}
    }
    call(231,0,0,0);__builtin_unreachable();
}
